//! Process-level Skills protocol coverage.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use yunxi_kernel::{PluginCommand, PluginId};
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{
    CapabilityDescriptor, GrantKind, SkillListRequest, SkillListResult, TOOL_SKILLS_LIST_OPERATION,
    capabilities,
};
use yunxi_tool_skills::SKILLS_PLUGIN_ID;

fn temp_root() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("yunxi-skills-process-{stamp}"));
    fs::create_dir_all(&root).expect("root");
    root
}

#[test]
fn child_process_lists_a_minimal_skill_without_frontmatter() {
    let root = temp_root();
    let skill = root.join("review");
    fs::create_dir_all(&skill).expect("skill");
    fs::write(skill.join("SKILL.md"), "review instructions\n").expect("skill file");
    fs::write(
        skill.join("tools.json"),
        r#"[{"name":"check","description":"Inspect metadata","input_schema":{"type":"object"}}]"#,
    )
    .expect("tools file");

    let mut host = ProcessPluginHost::new();
    let id = PluginId::new(SKILLS_PLUGIN_ID).expect("plugin id");
    let capability =
        CapabilityDescriptor::new(capabilities::TOOL_SKILLS, capabilities::TOOL_SKILLS_VERSION)
            .expect("capability");
    let command = PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-tool-skills"))
        .clear_environment()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env(
            "SystemRoot",
            std::env::var_os("SystemRoot").expect("SystemRoot"),
        )
        .env("WINDIR", std::env::var_os("WINDIR").expect("WINDIR"))
        .env("TEMP", std::env::var_os("TEMP").expect("TEMP"))
        .env("TMP", std::env::var_os("TMP").expect("TMP"))
        .env("YUNXI_NEXT_SKILLS_ROOT", root.as_os_str());
    host.launch(
        PluginLaunch::new(id, command)
            .with_required_grants([GrantKind::WorkspaceRead])
            .with_display_name("Skills process fixture"),
    )
    .expect("launch Skills process");

    let result = host
        .invoke::<_, SkillListResult>(
            &capability,
            TOOL_SKILLS_LIST_OPERATION,
            &SkillListRequest::new(),
        )
        .map_err(|error: PluginCallError| error.to_string())
        .expect("list Skills");
    assert_eq!(
        result.skills().len(),
        1,
        "warnings: {:?}",
        result.warnings()
    );
    assert_eq!(result.skills()[0].tools().len(), 1);
    host.shutdown();
    let _ignored = fs::remove_dir_all(root);
}
