use std::fs::{self, File};
use std::io::Write;
use std::time::Duration;
use tempfile::tempdir;
use threadlane_coding_agent::{
    CodingAgentOptions, HarnessSupervisor, PackageManager, SkillManager, SkillScope, TaskStatus,
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

fn write_wasi_package_fixture(source: &std::path::Path, extension: &str, wasm: &[u8]) {
    fs::create_dir_all(source).unwrap();
    fs::write(
        source.join("threadlane-package.json"),
        format!(
            r#"{{
                "id": "test-extension",
                "name": "Test Extension",
                "version": "1.0.0",
                "description": "test fixture",
                "extension": "{extension}"
            }}"#
        ),
    )
    .unwrap();
    fs::write(source.join("extension.wasm"), wasm).unwrap();
}

#[test]
fn package_install_lists_and_removes_project_wasi_extension() {
    let project = tempdir().unwrap();
    let source = tempdir().unwrap();
    write_wasi_package_fixture(source.path(), "extension.wasm", b"test wasm");

    let manager = PackageManager::new();
    let package = manager
        .install_from_local(source.path(), project.path())
        .unwrap();
    let module = project
        .path()
        .join(".threadlane/extensions/test-extension/extension.wasm");

    assert!(project
        .path()
        .join(".threadlane/extensions/test-extension/threadlane-package.json")
        .is_file());
    assert!(module.is_file());
    assert_eq!(package.id(), "test-extension");
    assert_eq!(package.name(), "Test Extension");
    assert_eq!(package.module_path(), module);
    assert!(package.is_enabled());
    assert_eq!(manager.list_packages(project.path()).len(), 1);

    manager
        .remove_package("test-extension", project.path())
        .unwrap();
    assert!(!module.exists());
}

#[test]
fn package_install_rejects_invalid_modules_without_creating_extensions() {
    for extension in [
        "../outside.wasm",
        "/tmp/outside.wasm",
        "extension.bin",
        "missing.wasm",
    ] {
        let project = tempdir().unwrap();
        let source_root = tempdir().unwrap();
        let source = source_root.path().join("source");
        write_wasi_package_fixture(&source, extension, b"test wasm");
        if extension == "../outside.wasm" {
            fs::write(source_root.path().join("outside.wasm"), b"outside wasm").unwrap();
        }

        assert!(
            PackageManager::new()
                .install_from_local(&source, project.path())
                .is_err(),
            "{extension} was accepted"
        );
        assert!(!project.path().join(".threadlane/extensions").exists());
    }
}

#[cfg(unix)]
#[test]
fn package_install_rejects_symlinked_extensions_destination() {
    let project = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let source = tempdir().unwrap();
    write_wasi_package_fixture(source.path(), "extension.wasm", b"test wasm");
    fs::create_dir(project.path().join(".threadlane")).unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        project.path().join(".threadlane/extensions"),
    )
    .unwrap();

    assert!(PackageManager::new()
        .install_from_local(source.path(), project.path())
        .is_err());
    assert!(!outside.path().join("test-extension").exists());
}

#[cfg(unix)]
#[test]
fn package_removal_rejects_symlinked_extensions_destination() {
    let project = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_package = outside.path().join("test-extension");
    let outside_module = outside_package.join("extension.wasm");
    fs::create_dir_all(&outside_package).unwrap();
    fs::write(&outside_module, b"outside wasm").unwrap();
    fs::create_dir(project.path().join(".threadlane")).unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        project.path().join(".threadlane/extensions"),
    )
    .unwrap();

    let result = PackageManager::new().remove_package("test-extension", project.path());

    assert!(outside_module.is_file(), "outside package was removed");
    assert!(result.is_err());
}

#[test]
fn package_install_preserves_existing_extension_when_replacement_is_invalid() {
    let project = tempdir().unwrap();
    let valid_source = tempdir().unwrap();
    let invalid_source_root = tempdir().unwrap();
    let invalid_source = invalid_source_root.path().join("source");
    write_wasi_package_fixture(valid_source.path(), "extension.wasm", b"original wasm");
    write_wasi_package_fixture(&invalid_source, "missing.wasm", b"replacement wasm");

    let manager = PackageManager::new();
    manager
        .install_from_local(valid_source.path(), project.path())
        .unwrap();
    assert!(manager
        .install_from_local(&invalid_source, project.path())
        .is_err());
    assert_eq!(
        fs::read(
            project
                .path()
                .join(".threadlane/extensions/test-extension/extension.wasm")
        )
        .unwrap(),
        b"original wasm"
    );
}

#[test]
fn package_list_ignores_hidden_replacement_backups() {
    let project = tempdir().unwrap();
    let source = tempdir().unwrap();
    write_wasi_package_fixture(source.path(), "extension.wasm", b"test wasm");

    let manager = PackageManager::new();
    manager
        .install_from_local(source.path(), project.path())
        .unwrap();
    let extensions = project.path().join(".threadlane/extensions");
    let backup = extensions.join(".test-extension.backup-test");
    fs::create_dir(&backup).unwrap();
    fs::copy(
        extensions.join("test-extension/threadlane-package.json"),
        backup.join("threadlane-package.json"),
    )
    .unwrap();
    fs::copy(
        extensions.join("test-extension/extension.wasm"),
        backup.join("extension.wasm"),
    )
    .unwrap();

    assert_eq!(manager.list_packages(project.path()).len(), 1);
}
