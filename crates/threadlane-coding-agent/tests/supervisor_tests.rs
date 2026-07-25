use std::fs::{self, File};
use std::io::Write;
use std::time::Duration;
use tempfile::tempdir;
use threadlane_coding_agent::{
    CodingAgentOptions, FullTrustRunner, HarnessSupervisor, PackageManager, SkillManager,
    SkillScope, TaskStatus, TrustStore,
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

#[test]
fn test_full_trust_revision_approval() {
    let global_dir = tempdir().unwrap();
    let trust_file = global_dir.path().join("state/trust.json");

    let exe_dir = tempdir().unwrap();
    let exe_path = exe_dir.path().join("dummy_extension.sh");
    {
        let mut f = File::create(&exe_path).unwrap();
        writeln!(f, "#!/bin/sh\necho '{{\"status\": \"ok\"}}'").unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&exe_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&exe_path, perms).unwrap();
    }

    let runner = FullTrustRunner::new("pkg-1".into(), exe_path.clone()).unwrap();
    let rev = runner.revision.clone();

    let err = runner.execute_request("{}", &trust_file);
    assert!(err.is_err());
    assert!(err.unwrap_err().contains("Security Denial"));

    let mut store = TrustStore::load_from_file(&trust_file);
    store.approve("pkg-1".into(), rev.clone());
    store.save_to_file(&trust_file).unwrap();

    let res = runner.execute_request("{}", &trust_file);
    assert!(res.is_ok());

    store.revoke("pkg-1");
    store.save_to_file(&trust_file).unwrap();
    assert!(runner.execute_request("{}", &trust_file).is_err());
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

        assert!(
            PackageManager::new()
                .install_from_local(&source, project.path())
                .is_err(),
            "{extension} was accepted"
        );
        assert!(!project.path().join(".threadlane/extensions").exists());
    }
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
