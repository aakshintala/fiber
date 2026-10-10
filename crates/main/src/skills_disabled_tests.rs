//! The `skills.disabled` re-read loads the configuration as at session
//! start, `-c` overrides included, and reports a failure with the error's
//! own code.

use super::skills_disabled_reader;

struct Home {
    _held: fakes::TempDir,
    home: std::path::PathBuf,
    workspace: std::path::PathBuf,
}

impl Home {
    fn new() -> Self {
        let held = fakes::TempDir::new("fiber-skills-disabled-reader");
        let home = held.path().join("home");
        let workspace = held.path().join("workspace");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        Self {
            _held: held,
            home,
            workspace,
        }
    }

    fn reader(&self, overrides: Vec<String>) -> r#loop::DisabledReader {
        skills_disabled_reader(
            self.home.clone(),
            self.workspace.clone(),
            config::ProjectKey::new("test").unwrap(),
            overrides,
        )
    }

    fn write_global(&self, text: &str) {
        std::fs::write(self.home.join("config.json"), text).unwrap();
    }
}

#[test]
fn a_name_written_after_the_reader_was_made_is_returned() {
    let home = Home::new();
    home.write_global("{}");
    let reader = home.reader(Vec::new());
    assert_eq!(reader().unwrap(), Vec::<String>::new());
    home.write_global(r#"{"skills": {"disabled": ["late"]}}"#);
    assert_eq!(reader().unwrap(), vec!["late".to_owned()]);
}

#[test]
fn a_command_line_override_is_included() {
    let home = Home::new();
    home.write_global("{}");
    let reader = home.reader(vec!["skills.disabled=[\"late\"]".into()]);
    assert_eq!(reader().unwrap(), vec!["late".to_owned()]);
}

#[test]
fn invalid_json_fails_with_the_config_invalid_code() {
    let home = Home::new();
    home.write_global("{invalid");
    let reader = home.reader(Vec::new());
    let error = reader().unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::ConfigInvalid);
    assert!(
        error
            .message
            .starts_with("Fiber could not re-read skills.disabled: "),
        "{}",
        error.message
    );
    assert!(
        error
            .message
            .ends_with("The last list read stays in force."),
        "{}",
        error.message
    );
}
