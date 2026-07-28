use std::fs::{self, File};
use std::io::Write;
use std::time::Duration;
use tempfile::tempdir;
use threadlane_coding_agent::{
    CodingAgent, CodingAgentOptions, ExtensionManager, ExtensionScope, HarnessSupervisor,
    SkillManager, SkillScope, TaskStatus,
};

#[tokio::test]
async fn test_supervisor_multi_task_isolation() {
    let global_dir = tempdir().unwrap();

    let proj1_dir = tempdir().unwrap();
    let proj2_dir = tempdir().unwrap();

    let supervisor = HarnessSupervisor::new(global_dir.path().to_path_buf());

    let proj1 = supervisor.register_project(proj1_dir.path()).unwrap();
    let proj2 = supervisor.register_project(proj2_dir.path()).unwrap();

    assert_ne!(proj1.id, proj2.id);

    let opts1 = CodingAgentOptions {
        api_key: "test_key".into(),
        account_id: None,
        model: "gpt-4o".into(),
        work_dir: proj1_dir.path().to_path_buf(),
        session_file: None,
        system_prompt: Default::default(),
    };

    let opts2 = CodingAgentOptions {
        api_key: "test_key".into(),
        account_id: None,
        model: "gpt-4o".into(),
        work_dir: proj2_dir.path().to_path_buf(),
        session_file: None,
        system_prompt: Default::default(),
    };

    let task1_id = supervisor.create_task(&proj1.id, None, opts1).unwrap();
    let task2_id = supervisor.create_task(&proj2.id, None, opts2).unwrap();

    assert_ne!(task1_id, task2_id);

    let t1_tasks = supervisor.list_tasks_for_project(&proj1.id);
    let t2_tasks = supervisor.list_tasks_for_project(&proj2.id);

    assert_eq!(t1_tasks.len(), 1);
    assert_eq!(t1_tasks[0].id, task1_id);

    assert_eq!(t2_tasks.len(), 1);
    assert_eq!(t2_tasks[0].id, task2_id);

    assert_ne!(t1_tasks[0].session_file, t2_tasks[0].session_file);
}

#[tokio::test]
async fn test_supervisor_submit_updates_status_and_forwards_events() {
    let global_dir = tempdir().unwrap();
    let project_dir = tempdir().unwrap();
    let supervisor = HarnessSupervisor::new(global_dir.path().to_path_buf());
    let project = supervisor.register_project(project_dir.path()).unwrap();
    let task_id = supervisor
        .create_task(
            &project.id,
            None,
            CodingAgentOptions {
                api_key: "test_key".into(),
                account_id: None,
                model: "gpt-4o".into(),
                work_dir: project_dir.path().to_path_buf(),
                session_file: None,
                system_prompt: Default::default(),
            },
        )
        .unwrap();
    let mut events = supervisor.subscribe();

    supervisor
        .submit_input(&task_id, "hello".to_string())
        .unwrap();
    assert_eq!(
        supervisor.get_task_status(&task_id),
        Some(TaskStatus::Running)
    );

    let forwarded = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(forwarded.task_id(), task_id);
    assert_eq!(forwarded.project_id(), project.id);
}

#[test]
fn test_skill_discovery_and_precedence() {
    let dir = tempdir().unwrap();
    let proj_skills = dir.path().join(".agents/skills/test-skill");
    fs::create_dir_all(&proj_skills).unwrap();

    let skill_file = proj_skills.join("SKILL.md");
    let mut f = File::create(&skill_file).unwrap();
    writeln!(
        f,
        "---\nname: test-skill\ndescription: A test skill\ntags: [test, mock]\n---\nInstruction step 1"
    )
    .unwrap();

    let home_dir = tempdir().unwrap();
    let mut mgr = SkillManager::new();
    mgr.discover_skills_with_home(Some(dir.path()), Some(home_dir.path()));

    let list = mgr.list_skills();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "test-skill");
    assert_eq!(list[0].scope, SkillScope::ProjectAgents);

    let instructions = mgr.get_skill_instructions("test-skill").unwrap();
    assert_eq!(instructions, "Instruction step 1");
}

fn push_unsigned_leb(mut value: u32, bytes: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn push_signed_leb(mut value: i64, bytes: &mut Vec<u8>) {
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        bytes.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

fn push_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
    wasm.push(id);
    push_unsigned_leb(payload.len() as u32, wasm);
    wasm.extend_from_slice(payload);
}

fn manifest_wasm_with_commands(name: &str, version: &str, commands: &[&str]) -> Vec<u8> {
    let commands = commands
        .iter()
        .map(|command| {
            serde_json::json!({
                "name": command,
                "description": format!("{command} command"),
            })
        })
        .collect::<Vec<_>>();
    let manifest = serde_json::json!({
        "api_version": 1,
        "name": name,
        "version": version,
        "description": "test fixture",
        "commands": commands,
    })
    .to_string();
    let mut wasm = b"\0asm\x01\0\0\0".to_vec();
    push_section(&mut wasm, 1, &[1, 0x60, 0, 1, 0x7e]);
    push_section(&mut wasm, 3, &[1, 0]);
    push_section(&mut wasm, 5, &[1, 0, 1]);

    let mut exports = vec![2];
    push_unsigned_leb("extension_info".len() as u32, &mut exports);
    exports.extend_from_slice(b"extension_info");
    exports.extend_from_slice(&[0, 0]);
    push_unsigned_leb("memory".len() as u32, &mut exports);
    exports.extend_from_slice(b"memory");
    exports.extend_from_slice(&[2, 0]);
    push_section(&mut wasm, 7, &exports);

    let mut body = vec![0, 0x42];
    push_signed_leb(manifest.len() as i64, &mut body);
    body.push(0x0b);
    let mut code = vec![1];
    push_unsigned_leb(body.len() as u32, &mut code);
    code.extend_from_slice(&body);
    push_section(&mut wasm, 10, &code);

    let mut data = vec![1, 0, 0x41, 0, 0x0b];
    push_unsigned_leb(manifest.len() as u32, &mut data);
    data.extend_from_slice(manifest.as_bytes());
    push_section(&mut wasm, 11, &data);
    wasm
}

fn manifest_wasm(name: &str, version: &str) -> Vec<u8> {
    manifest_wasm_with_commands(name, version, &[])
}

#[tokio::test]
async fn existing_coding_agent_reload_observes_replaced_extension_command() {
    let project = tempdir().unwrap();
    let source_dir = tempdir().unwrap();
    let source = source_dir.path().join("live-reload.wasm");
    fs::write(
        &source,
        manifest_wasm_with_commands("live_reload_ext", "1.0.0", &["before"]),
    )
    .unwrap();
    let manager = ExtensionManager::new(None, Some(project.path().to_path_buf()));
    manager
        .install_from_wasm(&source, ExtensionScope::Project)
        .unwrap();
    let mut agent = CodingAgent::new(CodingAgentOptions {
        api_key: "test_key".into(),
        account_id: None,
        model: "gpt-4o".into(),
        work_dir: project.path().to_path_buf(),
        session_file: None,
        system_prompt: Default::default(),
    });
    let initial = agent
        .wasi_extensions
        .extension_manifest("live_reload_ext")
        .unwrap();
    assert_eq!(initial.version, "1.0.0");
    assert_eq!(initial.commands[0].name, "before");

    fs::write(
        &source,
        manifest_wasm_with_commands("live_reload_ext", "2.0.0", &["after"]),
    )
    .unwrap();
    manager
        .install_from_wasm(&source, ExtensionScope::Project)
        .unwrap();

    let loaded = agent.reload_extensions().await.unwrap();
    let reloaded = agent
        .wasi_extensions
        .extension_manifest("live_reload_ext")
        .unwrap();
    assert!(loaded >= 1);
    assert_eq!(reloaded.version, "2.0.0");
    assert_eq!(reloaded.commands[0].name, "after");
}

#[test]
fn scoped_wasi_install_places_and_replaces_loose_modules() {
    let global_threadlane = tempdir().unwrap();
    let project = tempdir().unwrap();
    let source_dir = tempdir().unwrap();
    let global_source = source_dir.path().join("global.wasm");
    let project_source = source_dir.path().join("project.wasm");
    let replacement_source = source_dir.path().join("replacement.wasm");
    fs::write(&global_source, manifest_wasm("shared_ext", "1.0.0")).unwrap();
    fs::write(&project_source, manifest_wasm("project_ext", "1.0.0")).unwrap();
    let replacement = manifest_wasm("shared_ext", "2.0.0");
    fs::write(&replacement_source, &replacement).unwrap();

    let manager = ExtensionManager::new(
        Some(global_threadlane.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    );
    let global = manager
        .install_from_wasm(&global_source, ExtensionScope::Global)
        .unwrap();
    let project_record = manager
        .install_from_wasm(&project_source, ExtensionScope::Project)
        .unwrap();
    let replaced = manager
        .install_from_wasm(&replacement_source, ExtensionScope::Global)
        .unwrap();

    let global_path = global_threadlane
        .path()
        .canonicalize()
        .unwrap()
        .join("extensions/shared_ext.wasm");
    let project_path = project
        .path()
        .canonicalize()
        .unwrap()
        .join(".threadlane/extensions/project_ext.wasm");
    assert_eq!(global.id(), "shared_ext");
    assert_eq!(global.name(), "shared_ext");
    assert_eq!(global.version(), "1.0.0");
    assert_eq!(global.scope(), ExtensionScope::Global);
    assert_eq!(global.module_path(), global_path);
    assert_eq!(project_record.scope(), ExtensionScope::Project);
    assert_eq!(project_record.module_path(), project_path);
    assert_eq!(replaced.version(), "2.0.0");
    assert_eq!(fs::read(global_path).unwrap(), replacement);
}

#[test]
fn scoped_wasi_toggle_markers_are_independent() {
    let global_threadlane = tempdir().unwrap();
    let project = tempdir().unwrap();
    let source_dir = tempdir().unwrap();
    let global_source = source_dir.path().join("global.wasm");
    let project_source = source_dir.path().join("project.wasm");
    fs::write(&global_source, manifest_wasm("shared_ext", "1.0.0")).unwrap();
    fs::write(&project_source, manifest_wasm("shared_ext", "2.0.0")).unwrap();

    let manager = ExtensionManager::new(
        Some(global_threadlane.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    );
    manager
        .install_from_wasm(&global_source, ExtensionScope::Global)
        .unwrap();
    manager
        .install_from_wasm(&project_source, ExtensionScope::Project)
        .unwrap();

    let records = manager.discover();
    let global = records
        .iter()
        .find(|record| record.scope() == ExtensionScope::Global)
        .unwrap();
    let project_record = records
        .iter()
        .find(|record| record.scope() == ExtensionScope::Project)
        .unwrap();
    assert!(!global.is_effective());
    assert!(project_record.is_effective());

    manager.set_enabled(project_record, false).unwrap();
    assert!(project_record
        .module_path()
        .with_extension("wasm.disabled")
        .is_file());
    let records = manager.discover();
    let global = records
        .iter()
        .find(|record| record.scope() == ExtensionScope::Global)
        .unwrap();
    let project_record = records
        .iter()
        .find(|record| record.scope() == ExtensionScope::Project)
        .unwrap();
    assert!(global.is_enabled());
    assert!(global.is_effective());
    assert!(!project_record.is_enabled());
    assert!(!project_record.is_effective());

    manager.set_enabled(global, false).unwrap();
    assert!(global
        .module_path()
        .with_extension("wasm.disabled")
        .is_file());
    manager.set_enabled(project_record, true).unwrap();
    assert!(!project_record
        .module_path()
        .with_extension("wasm.disabled")
        .exists());
    let records = manager.discover();
    assert!(records
        .iter()
        .find(|record| record.scope() == ExtensionScope::Project)
        .unwrap()
        .is_effective());
    assert!(!records
        .iter()
        .find(|record| record.scope() == ExtensionScope::Global)
        .unwrap()
        .is_effective());
}

#[test]
fn scoped_wasi_remove_handles_loose_and_managed_layouts() {
    let global_threadlane = tempdir().unwrap();
    let project = tempdir().unwrap();
    let source_dir = tempdir().unwrap();
    let loose_source = source_dir.path().join("loose.wasm");
    fs::write(&loose_source, manifest_wasm("loose_ext", "1.0.0")).unwrap();
    let managed_dir = global_threadlane.path().join("extensions/legacy-package");
    fs::create_dir_all(&managed_dir).unwrap();
    fs::write(
        managed_dir.join("extension.wasm"),
        manifest_wasm("managed_ext", "1.0.0"),
    )
    .unwrap();

    let manager = ExtensionManager::new(
        Some(global_threadlane.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    );
    manager
        .install_from_wasm(&loose_source, ExtensionScope::Project)
        .unwrap();
    let records = manager.discover();
    let loose = records
        .iter()
        .find(|record| record.id() == "loose_ext")
        .unwrap();
    let managed = records
        .iter()
        .find(|record| record.id() == "legacy-package")
        .unwrap();
    manager.set_enabled(loose, false).unwrap();
    manager.set_enabled(managed, false).unwrap();
    let loose_marker = loose.module_path().with_extension("wasm.disabled");
    let managed_marker = managed.module_path().with_extension("wasm.disabled");
    assert!(loose_marker.is_file());
    assert!(managed_marker.is_file());

    manager.remove(loose).unwrap();
    manager.remove(managed).unwrap();

    assert!(!loose.module_path().exists());
    assert!(!loose_marker.exists());
    assert!(!managed_dir.exists());
}

#[test]
fn scoped_wasi_install_requires_manifest_and_preserves_existing_module() {
    let global_threadlane = tempdir().unwrap();
    let project = tempdir().unwrap();
    let source_dir = tempdir().unwrap();
    let valid_source = source_dir.path().join("valid.wasm");
    let missing_manifest_source = source_dir.path().join("missing-manifest.wasm");
    let invalid_source = source_dir.path().join("invalid.wasm");
    let original = manifest_wasm("stable_ext", "1.0.0");
    fs::write(&valid_source, &original).unwrap();
    fs::write(&missing_manifest_source, b"\0asm\x01\0\0\0").unwrap();
    fs::write(&invalid_source, b"not wasm").unwrap();

    let manager = ExtensionManager::new(
        Some(global_threadlane.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    );
    let installed = manager
        .install_from_wasm(&valid_source, ExtensionScope::Project)
        .unwrap();

    let missing_manifest = manager
        .install_from_wasm(&missing_manifest_source, ExtensionScope::Project)
        .unwrap_err();
    assert!(missing_manifest.contains("extension_info"));
    assert!(manager
        .install_from_wasm(&invalid_source, ExtensionScope::Project)
        .is_err());
    assert_eq!(fs::read(installed.module_path()).unwrap(), original);
    assert!(!project
        .path()
        .join(".threadlane/extensions/unnamed_wasi_ext.wasm")
        .exists());
}

#[test]
fn scoped_wasi_install_migrates_same_name_managed_layout() {
    let global_threadlane = tempdir().unwrap();
    let project = tempdir().unwrap();
    let source_dir = tempdir().unwrap();
    let replacement_source = source_dir.path().join("replacement.wasm");
    let managed_dir = global_threadlane.path().join("extensions/legacy-package");
    fs::create_dir_all(&managed_dir).unwrap();
    fs::write(
        managed_dir.join("extension.wasm"),
        manifest_wasm("shared_ext", "1.0.0"),
    )
    .unwrap();
    fs::write(&replacement_source, manifest_wasm("shared_ext", "2.0.0")).unwrap();

    let manager = ExtensionManager::new(
        Some(global_threadlane.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    );
    let replacement = manager
        .install_from_wasm(&replacement_source, ExtensionScope::Global)
        .unwrap();
    let records = manager
        .discover()
        .into_iter()
        .filter(|record| record.scope() == ExtensionScope::Global && record.name() == "shared_ext")
        .collect::<Vec<_>>();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id(), "shared_ext");
    assert_eq!(records[0].version(), "2.0.0");
    assert_eq!(replacement.module_path(), records[0].module_path());
    assert!(!managed_dir.exists());
}

#[test]
fn scoped_wasi_managed_migration_preserves_disabled_state() {
    let global_threadlane = tempdir().unwrap();
    let project = tempdir().unwrap();
    let source_dir = tempdir().unwrap();
    let replacement_source = source_dir.path().join("replacement.wasm");
    let managed_dir = global_threadlane.path().join("extensions/legacy-package");
    let managed_module = managed_dir.join("extension.wasm");
    fs::create_dir_all(&managed_dir).unwrap();
    fs::write(&managed_module, manifest_wasm("shared_ext", "1.0.0")).unwrap();
    fs::write(managed_module.with_extension("wasm.disabled"), []).unwrap();
    fs::write(&replacement_source, manifest_wasm("shared_ext", "2.0.0")).unwrap();

    let manager = ExtensionManager::new(
        Some(global_threadlane.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    );
    let replacement = manager
        .install_from_wasm(&replacement_source, ExtensionScope::Global)
        .unwrap();

    assert!(!replacement.is_enabled());
    assert!(replacement
        .module_path()
        .with_extension("wasm.disabled")
        .is_file());
    assert!(!managed_dir.exists());
}

#[test]
fn scoped_wasi_discovery_rejects_unsafe_manifest_names() {
    let global_threadlane = tempdir().unwrap();
    let extensions = global_threadlane.path().join("extensions");
    fs::create_dir_all(&extensions).unwrap();
    fs::write(
        extensions.join("unsafe.wasm"),
        manifest_wasm("../escape", "1.0.0"),
    )
    .unwrap();
    let manager = ExtensionManager::new(Some(global_threadlane.path().to_path_buf()), None);

    assert!(manager.discover().is_empty());
}

#[test]
fn scoped_wasi_discovery_rejects_overlong_manifest_names() {
    let global_threadlane = tempdir().unwrap();
    let extensions = global_threadlane.path().join("extensions");
    fs::create_dir_all(&extensions).unwrap();
    fs::write(
        extensions.join("overlong.wasm"),
        manifest_wasm(&"a".repeat(129), "1.0.0"),
    )
    .unwrap();
    let manager = ExtensionManager::new(Some(global_threadlane.path().to_path_buf()), None);

    assert!(manager.discover().is_empty());
}

#[cfg(unix)]
#[test]
fn scoped_wasi_install_does_not_follow_dangling_staging_symlink() {
    let project = tempdir().unwrap();
    let source_dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let source = source_dir.path().join("safe.wasm");
    fs::write(&source, manifest_wasm("safe_ext", "1.0.0")).unwrap();
    let extensions = project.path().join(".threadlane/extensions");
    fs::create_dir_all(&extensions).unwrap();
    let outside_target = outside.path().join("escaped.wasm");
    let staged = extensions.join(format!(".safe_ext.staged-{}-0.wasm", std::process::id()));
    std::os::unix::fs::symlink(&outside_target, staged).unwrap();
    let manager = ExtensionManager::new(None, Some(project.path().to_path_buf()));

    let installed = manager
        .install_from_wasm(&source, ExtensionScope::Project)
        .unwrap();
    assert!(!outside_target.exists());
    assert_eq!(
        installed.module_path(),
        extensions.canonicalize().unwrap().join("safe_ext.wasm")
    );
    assert!(!fs::symlink_metadata(installed.module_path())
        .unwrap()
        .file_type()
        .is_symlink());
}

#[cfg(unix)]
#[test]
fn scoped_wasi_mutations_reject_symlink_escapes() {
    let global_threadlane = tempdir().unwrap();
    let project = tempdir().unwrap();
    let source_dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let source = source_dir.path().join("safe.wasm");
    let outside_module = outside.path().join("outside.wasm");
    fs::write(&source, manifest_wasm("safe_ext", "1.0.0")).unwrap();
    fs::write(&outside_module, manifest_wasm("outside_ext", "1.0.0")).unwrap();
    let manager = ExtensionManager::new(
        Some(global_threadlane.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    );
    manager
        .install_from_wasm(&source, ExtensionScope::Project)
        .unwrap();
    let record = manager.discover().pop().unwrap();
    fs::remove_file(record.module_path()).unwrap();
    std::os::unix::fs::symlink(&outside_module, record.module_path()).unwrap();

    assert!(manager.set_enabled(&record, false).is_err());
    assert!(manager.remove(&record).is_err());
    assert!(outside_module.is_file());

    let linked_project = tempdir().unwrap();
    fs::create_dir(linked_project.path().join(".threadlane")).unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        linked_project.path().join(".threadlane/extensions"),
    )
    .unwrap();
    let linked_manager = ExtensionManager::new(
        Some(global_threadlane.path().to_path_buf()),
        Some(linked_project.path().to_path_buf()),
    );
    assert!(linked_manager
        .install_from_wasm(&source, ExtensionScope::Project)
        .is_err());
    assert!(!outside.path().join("safe_ext.wasm").exists());
}
