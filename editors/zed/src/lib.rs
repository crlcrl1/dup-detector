use zed_extension_api::{self as zed, Command, LanguageServerId, Result, Worktree};

struct DupDetectorExtension;

impl zed::Extension for DupDetectorExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let command = worktree
            .which("dup-detector")
            .unwrap_or_else(|| "dup-detector".to_string());
        Ok(Command {
            command,
            args: vec!["lsp".to_string()],
            env: Default::default(),
        })
    }
}

zed::register_extension!(DupDetectorExtension);
