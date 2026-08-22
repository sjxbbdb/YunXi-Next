use std::io;
use std::process::Child;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{PluginFailure, PluginId, PluginSpec, PluginState};

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(crate) enum SupervisorCommand {
    Stop,
}

pub(crate) struct SupervisorEvent {
    pub id: PluginId,
    pub generation: u64,
    pub state: PluginState,
}

pub(crate) struct SupervisorHandle {
    pub commands: Sender<SupervisorCommand>,
    pub worker: JoinHandle<()>,
}

pub(crate) fn spawn_supervisor(
    spec: PluginSpec,
    generation: u64,
    events: Sender<SupervisorEvent>,
) -> io::Result<SupervisorHandle> {
    let (commands, command_receiver) = mpsc::channel();
    let thread_name = format!("yunxi-plugin-{}", spec.id());
    let worker = thread::Builder::new().name(thread_name).spawn(move || {
        supervise_process(spec, generation, command_receiver, events);
    })?;
    Ok(SupervisorHandle { commands, worker })
}

fn supervise_process(
    spec: PluginSpec,
    generation: u64,
    commands: Receiver<SupervisorCommand>,
    events: Sender<SupervisorEvent>,
) {
    let mut child = match spec.command().spawn() {
        Ok(child) => child,
        Err(error) => {
            send_state(
                &events,
                spec.id(),
                generation,
                PluginState::Failed(PluginFailure::Spawn {
                    message: error.to_string(),
                }),
            );
            return;
        }
    };

    if events
        .send(SupervisorEvent {
            id: spec.id().clone(),
            generation,
            state: PluginState::Running { pid: child.id() },
        })
        .is_err()
    {
        terminate_child(&mut child);
        return;
    }

    loop {
        match commands.recv_timeout(PROCESS_POLL_INTERVAL) {
            Ok(SupervisorCommand::Stop) | Err(RecvTimeoutError::Disconnected) => {
                terminate_child(&mut child);
                send_state(&events, spec.id(), generation, PluginState::Stopped);
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                send_state(
                    &events,
                    spec.id(),
                    generation,
                    PluginState::Failed(PluginFailure::UnexpectedExit {
                        code: status.code(),
                    }),
                );
                return;
            }
            Ok(None) => {}
            Err(error) => {
                terminate_child(&mut child);
                send_state(
                    &events,
                    spec.id(),
                    generation,
                    PluginState::Failed(PluginFailure::Monitor {
                        message: error.to_string(),
                    }),
                );
                return;
            }
        }
    }
}

fn send_state(
    events: &Sender<SupervisorEvent>,
    id: &PluginId,
    generation: u64,
    state: PluginState,
) {
    let _ignored = events.send(SupervisorEvent {
        id: id.clone(),
        generation,
        state,
    });
}

fn terminate_child(child: &mut Child) {
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }
    let _ignored = child.kill();
    let _ignored = child.wait();
}
