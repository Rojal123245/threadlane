//! Command palette state: slash commands definition and matching.

pub use threadlane_coding_agent::SlashCommandInfo as CommandInfo;

/// Built-in slash commands handled by the coding agent.
pub fn builtin_commands() -> Vec<CommandInfo> {
    threadlane_coding_agent::builtin_commands()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_command_is_discoverable() {
        let commands = builtin_commands();

        assert!(commands.iter().any(|command| command.name == "skill"));
        assert!(commands.iter().any(|command| command.name == "task"));
        assert!(!commands.iter().any(|command| command.name == "clear-plan"));
    }
}
