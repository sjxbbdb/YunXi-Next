use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use crate::supervisor::{SupervisorCommand, SupervisorEvent, SupervisorHandle, spawn_supervisor};
use crate::{KernelError, PluginFailure, PluginId, PluginSnapshot, PluginSpec, PluginState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelState {
    Running,
    ShuttingDown,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelSnapshot {
    state: KernelState,
    plugins: Vec<PluginSnapshot>,
}

impl KernelSnapshot {
    pub fn state(&self) -> KernelState {
        self.state
    }

    pub fn plugins(&self) -> &[PluginSnapshot] {
        &self.plugins
    }

    pub fn running_plugin_count(&self) -> usize {
        self.plugins
            .iter()
            .filter(|plugin| matches!(plugin.state(), PluginState::Running { .. }))
            .count()
    }

    pub fn failed_plugin_count(&self) -> usize {
        self.plugins
            .iter()
            .filter(|plugin| plugin.state().is_failed())
            .count()
    }
}

struct PluginSlot {
    spec: PluginSpec,
    state: PluginState,
    generation: u64,
    commands: Option<Sender<SupervisorCommand>>,
    worker: Option<JoinHandle<()>>,
}

impl PluginSlot {
    fn new(spec: PluginSpec) -> Self {
        Self {
            spec,
            state: PluginState::Registered,
            generation: 0,
            commands: None,
            worker: None,
        }
    }

    fn snapshot(&self) -> PluginSnapshot {
        PluginSnapshot::new(
            self.spec.id().clone(),
            self.spec.display_name().to_string(),
            self.state.clone(),
            self.generation,
        )
    }
}

pub struct YunxiKernel {
    state: KernelState,
    plugins: BTreeMap<PluginId, PluginSlot>,
    event_sender: Sender<SupervisorEvent>,
    event_receiver: Receiver<SupervisorEvent>,
}

impl YunxiKernel {
    pub fn new() -> Self {
        let (event_sender, event_receiver) = mpsc::channel();
        Self {
            state: KernelState::Running,
            plugins: BTreeMap::new(),
            event_sender,
            event_receiver,
        }
    }

    pub fn state(&self) -> KernelState {
        self.state
    }

    pub fn is_healthy(&self) -> bool {
        self.state == KernelState::Running
    }

    pub fn register(&mut self, spec: PluginSpec) -> Result<(), KernelError> {
        self.ensure_running()?;
        let id = spec.id().clone();
        if self.plugins.contains_key(&id) {
            return Err(KernelError::DuplicatePlugin { id });
        }
        self.plugins.insert(id, PluginSlot::new(spec));
        Ok(())
    }

    pub fn start(&mut self, id: &PluginId) -> Result<(), KernelError> {
        self.ensure_running()?;
        self.refresh();

        let (spec, generation, previous_worker) = {
            let slot = self
                .plugins
                .get_mut(id)
                .ok_or_else(|| KernelError::UnknownPlugin { id: id.clone() })?;
            if slot.state.is_active() {
                return Err(KernelError::PluginBusy {
                    id: id.clone(),
                    state: slot.state.clone(),
                });
            }
            slot.commands = None;
            let previous_worker = slot.worker.take();
            slot.generation = slot.generation.saturating_add(1);
            slot.state = PluginState::Starting;
            (slot.spec.clone(), slot.generation, previous_worker)
        };

        if let Some(worker) = previous_worker {
            let _ignored = worker.join();
        }

        let SupervisorHandle { commands, worker } =
            spawn_supervisor(spec, generation, self.event_sender.clone()).map_err(|error| {
                let failure = PluginFailure::Supervisor {
                    message: error.to_string(),
                };
                if let Some(slot) = self.plugins.get_mut(id) {
                    slot.state = PluginState::Failed(failure);
                }
                KernelError::SupervisorThread {
                    id: id.clone(),
                    message: error.to_string(),
                }
            })?;

        let slot = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| KernelError::UnknownPlugin { id: id.clone() })?;
        slot.commands = Some(commands);
        slot.worker = Some(worker);
        Ok(())
    }

    pub fn stop(&mut self, id: &PluginId) -> Result<(), KernelError> {
        self.ensure_running()?;
        self.refresh();
        let slot = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| KernelError::UnknownPlugin { id: id.clone() })?;

        match slot.state {
            PluginState::Registered => {
                slot.state = PluginState::Stopped;
                Ok(())
            }
            PluginState::Starting | PluginState::Running { .. } | PluginState::Stopping => {
                slot.state = PluginState::Stopping;
                let commands = slot
                    .commands
                    .as_ref()
                    .ok_or_else(|| KernelError::SupervisorUnavailable { id: id.clone() })?;
                commands
                    .send(SupervisorCommand::Stop)
                    .map_err(|_| KernelError::SupervisorUnavailable { id: id.clone() })
            }
            PluginState::Stopped | PluginState::Failed(_) => Ok(()),
        }
    }

    pub fn refresh(&mut self) -> Vec<PluginSnapshot> {
        let mut changed = Vec::new();
        while let Ok(event) = self.event_receiver.try_recv() {
            self.apply_event(event, &mut changed);
        }
        self.reap_finished_workers();
        changed
    }

    pub fn plugin(&self, id: &PluginId) -> Option<PluginSnapshot> {
        self.plugins.get(id).map(PluginSlot::snapshot)
    }

    pub fn snapshot(&self) -> KernelSnapshot {
        KernelSnapshot {
            state: self.state,
            plugins: self.plugins.values().map(PluginSlot::snapshot).collect(),
        }
    }

    pub fn shutdown(&mut self) {
        if self.state == KernelState::Stopped {
            return;
        }

        self.refresh();
        self.state = KernelState::ShuttingDown;
        for slot in self.plugins.values_mut() {
            if slot.state.is_active() {
                slot.state = PluginState::Stopping;
                if let Some(commands) = &slot.commands {
                    let _ignored = commands.send(SupervisorCommand::Stop);
                }
            } else if matches!(slot.state, PluginState::Registered) {
                slot.state = PluginState::Stopped;
            }
        }

        let ids: Vec<PluginId> = self.plugins.keys().cloned().collect();
        for id in ids {
            let worker = self
                .plugins
                .get_mut(&id)
                .and_then(|slot| slot.worker.take());
            if let Some(worker) = worker
                && worker.join().is_err()
                && let Some(slot) = self.plugins.get_mut(&id)
            {
                slot.state = PluginState::Failed(PluginFailure::Supervisor {
                    message: "supervisor thread panicked during shutdown".to_string(),
                });
            }
        }

        self.refresh();
        for slot in self.plugins.values_mut() {
            slot.commands = None;
            if slot.state.is_active() {
                slot.state = PluginState::Failed(PluginFailure::Supervisor {
                    message: "supervisor stopped without a terminal state".to_string(),
                });
            }
        }
        self.state = KernelState::Stopped;
    }

    fn ensure_running(&self) -> Result<(), KernelError> {
        if self.state == KernelState::Running {
            Ok(())
        } else {
            Err(KernelError::NotRunning)
        }
    }

    fn apply_event(&mut self, event: SupervisorEvent, changed: &mut Vec<PluginSnapshot>) {
        let Some(slot) = self.plugins.get_mut(&event.id) else {
            return;
        };
        if slot.generation != event.generation {
            return;
        }
        slot.state = event.state;
        if slot.state.is_terminal() {
            slot.commands = None;
        }
        changed.push(slot.snapshot());
    }

    fn reap_finished_workers(&mut self) {
        let finished_ids: Vec<PluginId> = self
            .plugins
            .iter()
            .filter(|(_, slot)| slot.worker.as_ref().is_some_and(JoinHandle::is_finished))
            .map(|(id, _)| id.clone())
            .collect();

        for id in finished_ids {
            let worker = self
                .plugins
                .get_mut(&id)
                .and_then(|slot| slot.worker.take());
            if let Some(worker) = worker
                && worker.join().is_err()
                && let Some(slot) = self.plugins.get_mut(&id)
            {
                slot.commands = None;
                slot.state = PluginState::Failed(PluginFailure::Supervisor {
                    message: "supervisor thread panicked".to_string(),
                });
            }
        }
    }
}

impl Default for YunxiKernel {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for YunxiKernel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginCommand;

    fn spec(id: &str) -> PluginSpec {
        PluginSpec::new(
            PluginId::new(id).expect("valid test plugin id"),
            PluginCommand::new("unused-test-command"),
        )
    }

    #[test]
    fn duplicate_plugin_ids_are_rejected() {
        let mut kernel = YunxiKernel::new();
        kernel.register(spec("yunxi.test")).expect("first plugin");
        let error = kernel
            .register(spec("yunxi.test"))
            .expect_err("duplicate plugin must fail");
        assert!(matches!(error, KernelError::DuplicatePlugin { .. }));
    }

    #[test]
    fn stopping_a_registered_plugin_never_spawns_it() {
        let mut kernel = YunxiKernel::new();
        let id = PluginId::new("yunxi.test").expect("valid plugin id");
        kernel.register(spec(id.as_str())).expect("register plugin");
        kernel.stop(&id).expect("stop plugin");
        assert_eq!(
            kernel.plugin(&id).expect("plugin snapshot").state(),
            &PluginState::Stopped
        );
    }
}
