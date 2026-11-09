use std::{fmt::Display, fs, io, os::unix::fs::OpenOptionsExt, path::Path};

use crate::{
    agent::{AgentError, AgentKind, ConversationState},
    isolation::{FileTransferSink, IsolationEngine},
    task::{Task, TaskService, TaskServiceError},
};

impl<E> TaskService<E>
where
    E: IsolationEngine + Clone + 'static,
{
    pub(crate) async fn collect_conversation(
        &self,
        isolation: &E,
        task: &Task,
        sandbox_id: &str,
        destination: &Path,
        progress: &dyn FileTransferSink,
    ) -> Result<(), TaskServiceError> {
        let (Some(kind), Some(conversation_id)) =
            (task.agent_kind, task.facts.agent_conversation_id.as_deref())
        else {
            return Ok(());
        };
        let state = self.agents.engine(kind).conversation_state(conversation_id);
        let exists = isolation
            .guest_regular_file_exists(sandbox_id.to_owned(), state.required_file().to_owned())
            .await
            .map_err(TaskServiceError::Isolation)?;
        if !exists {
            return Ok(());
        }
        match state {
            ConversationState::File(path) => {
                isolation
                    .collect_file(sandbox_id.to_owned(), path, destination.to_owned())
                    .await
                    .map_err(TaskServiceError::Isolation)?;
            }
            ConversationState::Directories { paths, .. } => {
                // Only the provider's task-local directories are eligible. Packing
                // CODEX_HOME would also carry the machine's login.
                let parts = tempfile::tempdir().map_err(conversation_error)?;
                for (index, path) in paths.iter().enumerate() {
                    isolation
                        .collect_files(
                            sandbox_id.to_owned(),
                            (*path).to_owned(),
                            Vec::new(),
                            parts.path().join(index.to_string()),
                            progress,
                        )
                        .await
                        .map_err(TaskServiceError::Isolation)?;
                }
                let file = fs::OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .mode(0o600)
                    .open(destination)
                    .map_err(conversation_error)?;
                let mut archive = tar::Builder::new(file);
                for index in 0..paths.len() {
                    archive
                        .append_path_with_name(
                            parts.path().join(index.to_string()),
                            index.to_string(),
                        )
                        .map_err(conversation_error)?;
                }
                archive.finish().map_err(conversation_error)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn deliver_conversation(
        &self,
        isolation: &E,
        sandbox_id: &str,
        kind: AgentKind,
        conversation_id: &str,
        source: &Path,
        progress: &dyn FileTransferSink,
    ) -> Result<(), TaskServiceError> {
        match self.agents.engine(kind).conversation_state(conversation_id) {
            ConversationState::File(path) => {
                isolation
                    .deliver_file(sandbox_id.to_owned(), source.to_owned(), path)
                    .await
                    .map_err(TaskServiceError::Isolation)?;
            }
            ConversationState::Directories { paths, .. } => {
                let parts = tempfile::tempdir().map_err(conversation_error)?;
                let mut received = vec![false; paths.len()];
                let mut archive =
                    tar::Archive::new(fs::File::open(source).map_err(conversation_error)?);
                for entry in archive.entries().map_err(conversation_error)? {
                    let mut entry = entry.map_err(conversation_error)?;
                    let name = entry.path().map_err(conversation_error)?;
                    let index = (0..paths.len())
                        .find(|index| name.as_ref() == Path::new(&index.to_string()));
                    let Some(index) = index.filter(|index| !received[*index]) else {
                        return Err(conversation_error(
                            "unexpected or duplicate conversation archive entry",
                        ));
                    };
                    if !entry.header().entry_type().is_file() {
                        return Err(conversation_error(
                            "conversation archive parts must be regular files",
                        ));
                    }
                    let mut file = fs::File::create(parts.path().join(index.to_string()))
                        .map_err(conversation_error)?;
                    io::copy(&mut entry, &mut file).map_err(conversation_error)?;
                    received[index] = true;
                }
                if received.contains(&false) {
                    return Err(conversation_error(
                        "conversation archive is missing native session state",
                    ));
                }
                for (index, path) in paths.iter().enumerate() {
                    isolation
                        .deliver_files(
                            sandbox_id.to_owned(),
                            parts.path().join(index.to_string()),
                            (*path).to_owned(),
                            progress,
                        )
                        .await
                        .map_err(TaskServiceError::Isolation)?;
                }
            }
        }
        Ok(())
    }
}

fn conversation_error(error: impl Display) -> TaskServiceError {
    TaskServiceError::Agent(AgentError::new(format!(
        "could not transfer the agent's conversation: {error}"
    )))
}
