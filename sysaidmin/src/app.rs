use std::collections::VecDeque;

use log::{debug, error, info, trace, warn};

use crate::allowlist::Allowlist;
use crate::api::AnthropicClient;
use crate::config::AppConfig;
use crate::executor::{ExecutionResult, Executor, FileEditOutcome};
use crate::parser;
use crate::session::SessionStore;
use crate::task::{Task, TaskDetail, TaskStatus};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum InputMode {
    Prompt,
    Logs,
}

pub struct App {
    pub tasks: Vec<Task>,
    pub selected: usize,
    pub input: String,
    pub input_mode: InputMode,
    pub logs: Vec<String>,
    pub summary: Option<String>,
    config: AppConfig,
    client: AnthropicClient,
    allowlist: Allowlist,
    executor: Executor,
    session: SessionStore,
    approval_queue: VecDeque<usize>,
}

impl App {
    pub fn new(
        config: AppConfig,
        client: AnthropicClient,
        allowlist: Allowlist,
        executor: Executor,
        session: SessionStore,
    ) -> Self {
        info!("Creating new App instance");
        debug!("App config: dry_run={}, offline_mode={}", config.dry_run, config.offline_mode);
        Self {
            tasks: Vec::new(),
            selected: 0,
            input: String::new(),
            input_mode: InputMode::Prompt,
            logs: Vec::new(),
            summary: None,
            config,
            client,
            allowlist,
            executor,
            session,
            approval_queue: VecDeque::new(),
        }
    }

    pub fn submit_prompt(&mut self) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() {
            warn!("Attempted to submit empty prompt");
            return;
        }
        info!("Submitting prompt: {}", prompt);
        self.log(format!("Requesting plan for: {}", prompt));
        
        trace!("Calling API client.plan()");
        match self.client.plan(&prompt) {
            Ok(response_text) => {
                info!("Received plan response ({} bytes)", response_text.len());
                trace!("Response preview: {}", response_text.chars().take(200).collect::<String>());
                
                trace!("Parsing plan JSON");
                match parser::parse_plan(&response_text, &self.config.default_shell) {
                    Ok(parsed) => {
                        info!("Plan parsed successfully: {} tasks", parsed.tasks.len());
                        self.summary = parsed.summary.clone();
                        self.tasks = parsed.tasks;
                        self.input.clear();
                        self.selected = 0;
                        
                        info!("Evaluating {} tasks against allowlist", self.tasks.len());
                        for (idx, task) in self.tasks.iter_mut().enumerate() {
                            trace!("Evaluating task {}: {}", idx, task.description);
                            match self.allowlist.evaluate(task) {
                                Ok(status) => {
                                    debug!("Task {} status: {:?}", idx, status);
                                    task.status = status;
                                }
                                Err(err) => {
                                    warn!("Task {} blocked: {}", idx, err);
                                    task.status = TaskStatus::Blocked(err.to_string());
                                }
                            }
                        }
                        
                        trace!("Persisting plan");
                        self.persist_plan();
                        
                        trace!("Enqueueing blocked tasks");
                        self.enqueue_blocked();
                        
                        self.log("Plan updated from SYSAIDMIN.");
                        info!("Running ready tasks");
                        self.run_ready_tasks();
                    }
                    Err(err) => {
                        error!("Failed parsing plan: {:?}", err);
                        self.log(format!("Failed parsing plan: {err:?}"));
                    }
                }
            }
            Err(err) => {
                error!("Failed requesting plan: {:?}", err);
                self.log(format!("Failed requesting plan: {err:?}"));
            }
        }
    }

    pub fn move_next(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.tasks.len() - 1);
    }

    pub fn move_prev(&mut self) {
        if self.selected == 0 {
            return;
        }
        self.selected -= 1;
    }

    fn log(&mut self, entry: impl Into<String>) {
        let line = entry.into();
        self.logs.push(line.clone());
        if self.logs.len() > self.config.history_limit {
            let excess = self.logs.len() - self.config.history_limit;
            self.logs.drain(0..excess);
        }
        if let Err(err) = self.session.append_log(&line) {
            self.logs.push(format!("(log write failed: {err})"));
        }
    }

    pub fn execute_selected(&mut self) {
        info!("Executing selected task (index: {})", self.selected);
        let (detail, description) = {
            let Some(task) = self.tasks.get_mut(self.selected) else {
                warn!("No task at selected index {}", self.selected);
                return;
            };
            let desc = task.description.clone();
            if !matches!(task.status, TaskStatus::Ready | TaskStatus::Proposed) {
                warn!("Task {} not ready for execution (status: {:?})", self.selected, task.status);
                return;
            }
            info!("Executing task: {}", desc);
            task.status = TaskStatus::Running;
            (task.detail.clone(), desc)
        };

        match detail {
            TaskDetail::Command(cmd) => {
                info!("Running command: {} (shell: {})", cmd.command, cmd.shell);
                trace!("Command details: cwd={:?}, requires_root={}", cmd.cwd, cmd.requires_root);
                match self.executor.run_command(&cmd) {
                    Ok(result) => {
                        info!("Command executed successfully: exit_code={}, stdout_len={}, stderr_len={}", 
                              result.status, result.stdout.len(), result.stderr.len());
                        self.mark_complete_with_log(
                            format!("Executed '{}' exit {}", description, result.status),
                            Some(result),
                            None,
                        );
                    }
                    Err(err) => {
                        error!("Command execution failed: {:?}", err);
                        self.log(format!("Execution failed: {err:?}"));
                        self.set_blocked(format!("execution failed: {err}"));
                    }
                }
                return;
            }
            TaskDetail::FileEdit(edit) => {
                let path_str = edit.path.as_deref().unwrap_or("<no path>");
                info!("Applying file edit: {} ({} bytes)", path_str, edit.new_text.len());
                match self.executor.apply_file_edit(&edit) {
                    Ok(outcome) => {
                        info!("File edit successful: {}", outcome.path.display());
                        if let Some(ref backup) = outcome.backup_path {
                            info!("Backup created: {}", backup.display());
                        }
                        self.mark_complete_with_log(
                            format!(
                                "Wrote {} (backup: {})",
                                outcome.path.display(),
                                outcome
                                    .backup_path
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|| "none".into())
                            ),
                            None,
                            Some(outcome),
                        );
                    }
                    Err(err) => {
                        error!("File edit failed: {:?}", err);
                        self.log(format!("Edit failed: {err:?}"));
                        self.set_blocked(format!("edit failed: {err}"));
                    }
                }
                return;
            }
            TaskDetail::Note { details } => {
                info!("Processing note task: {}", details);
                self.log(format!("Note: {}", details));
                if let Some(task) = self.tasks.get_mut(self.selected) {
                    task.status = TaskStatus::Complete;
                }
                return;
            }
        }
    }

    fn mark_complete_with_log(
        &mut self,
        summary: String,
        exec: Option<ExecutionResult>,
        edit: Option<FileEditOutcome>,
    ) {
        if let Some(task) = self.tasks.get_mut(self.selected) {
            task.status = TaskStatus::Complete;
            if let Some(result) = &exec {
                task.annotations.push(format!("exit {}", result.status));
            }
            if let Some(edit) = &edit {
                task.annotations
                    .push(format!("written {}", edit.path.display()));
            }
        }
        self.log(summary);
        if let Some(result) = exec {
            if !result.stdout.trim().is_empty() {
                self.log(format!("stdout: {}", truncate(&result.stdout)));
            }
            if !result.stderr.trim().is_empty() {
                self.log(format!("stderr: {}", truncate(&result.stderr)));
            }
        }
    }

    fn set_blocked(&mut self, reason: String) {
        if let Some(task) = self.tasks.get_mut(self.selected) {
            task.status = TaskStatus::Blocked(reason.clone());
        }
        self.log(reason);
    }

    fn persist_plan(&mut self) {
        if let Err(err) = self
            .session
            .write_plan(self.summary.as_deref(), &self.tasks)
        {
            self.log(format!("Failed to export plan: {err}"));
        }
    }

    pub fn dry_run(&self) -> bool {
        self.config.dry_run
    }

    fn run_ready_tasks(&mut self) {
        let task_count = self.tasks.len();
        for idx in 0..task_count {
            if matches!(
                self.tasks.get(idx).map(|t| &t.status),
                Some(TaskStatus::Ready)
            ) {
                self.selected = idx;
                self.log(format!("Auto-executing '{}'", self.tasks[idx].description));
                self.execute_selected();
            }
        }
    }

    pub fn has_pending_approval(&self) -> bool {
        !self.approval_queue.is_empty()
    }

    pub fn pending_approval_message(&self) -> Option<String> {
        self.approval_queue
            .front()
            .and_then(|idx| self.tasks.get(*idx))
            .and_then(|task| {
                if let TaskStatus::Blocked(reason) = &task.status {
                    Some(format!(
                        "Allow blocked task '{}'?\nReason: {}\nPress 'y' to allow, 'n' to skip.",
                        task.description, reason
                    ))
                } else {
                    None
                }
            })
    }

    pub fn approve_current_blocked(&mut self) {
        if let Some(idx) = self.approval_queue.pop_front() {
            if idx < self.tasks.len() {
                self.selected = idx;
                if let Some(task) = self.tasks.get_mut(idx) {
                    task.status = TaskStatus::Ready;
                }
                self.log(format!(
                    "Approved blocked task '{}'; running now.",
                    self.tasks[idx].description
                ));
                self.execute_selected();
            }
        }
    }

    pub fn reject_current_blocked(&mut self) {
        if let Some(idx) = self.approval_queue.pop_front() {
            let message = self
                .tasks
                .get(idx)
                .map(|task| task.description.clone())
                .unwrap_or_else(|| "unknown task".into());
            self.log(format!(
                "Skipped blocked task '{}'; leaving blocked.",
                message
            ));
        }
    }

    fn enqueue_blocked(&mut self) {
        self.approval_queue.clear();
        for (idx, task) in self.tasks.iter().enumerate() {
            if matches!(task.status, TaskStatus::Blocked(_)) {
                self.approval_queue.push_back(idx);
            }
        }
        if let Some(message) = self.pending_approval_message() {
            self.log(message);
        }
    }
}

fn truncate(text: &str) -> String {
    const LIMIT: usize = 200;
    if text.len() <= LIMIT {
        text.to_string()
    } else {
        format!("{}…", &text[..LIMIT])
    }
}
