use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use anyhow::Error;
use chrono::Utc;
use log::{debug, error, info, trace, warn};

use crate::allowlist::Allowlist;
use crate::api::AnthropicClient;
use crate::config::AppConfig;
use crate::conversation::{ConversationEntry, ConversationLogger};
use crate::executor::{ExecutionResult, Executor, FileEditOutcome};
use crate::history::HistoryWriter;
use crate::parser;
use crate::session::SessionStore;
use crate::task::{Task, TaskDetail, TaskStatus};
use crate::tui::{Message, MessageType};

pub struct App {
    pub tasks: Vec<Task>,
    pub selected: usize,
    pub input: String,
    pub logs: Vec<String>,
    pub summary: Option<String>,
    pub execution_results: HashMap<usize, ExecutionResult>, // task index -> execution result
    pub analysis_result: Option<String>,                    // Synthesis/analysis result from LLM
    pub analysis_scroll_offset: usize,                      // Scroll offset for analysis display
    pub is_loading_plan: bool,   // True when waiting for plan API response
    pub spinner_frame: usize,    // Current spinner animation frame
    last_prompt: Option<String>, // Store last prompt for synthesis detection
    messages: Vec<Message>,      // Message stream for TUI
    message_scroll_offset: usize, // Scroll offset for message stream
    config: AppConfig,
    client: AnthropicClient,
    allowlist: Allowlist,
    executor: Executor,
    session: SessionStore,
    approval_queue: VecDeque<usize>,
    conversation: ConversationLogger,
    history: HistoryWriter,
    plan_receiver: Option<Receiver<PlanResponse>>,
}

enum PlanResponse {
    Success(String),
    Error(String),
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
        debug!(
            "App config: dry_run={}, offline_mode={}",
            config.dry_run, config.offline_mode
        );

        // Initialize conversation logger
        let conversation_path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("sysaidmin.conversation.jsonl");
        let conversation = ConversationLogger::new(conversation_path.clone()).unwrap_or_else(|e| {
            warn!("Failed to create conversation logger: {}", e);
            // Create a dummy logger that does nothing
            ConversationLogger::new(PathBuf::from("/dev/null"))
                .expect("Failed to create dummy conversation logger")
        });
        info!(
            "Conversation logger initialized at: {}",
            conversation_path.display()
        );

        // Initialize history writer
        let history_path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("sysaidmin.history.sh");
        let history = HistoryWriter::new(history_path.clone()).unwrap_or_else(|e| {
            warn!("Failed to create history writer: {}", e);
            // Create a dummy writer that does nothing
            HistoryWriter::new(PathBuf::from("/dev/null"))
                .expect("Failed to create dummy history writer")
        });
        info!(
            "History writer initialized at: {}",
            history_path.display()
        );

        Self {
            tasks: Vec::new(),
            selected: 0,
            input: String::new(),
            logs: Vec::new(),
            summary: None,
            execution_results: HashMap::new(),
            analysis_result: None,
            analysis_scroll_offset: 0,
            is_loading_plan: false,
            spinner_frame: 0,
            last_prompt: None,
            messages: Vec::new(),
            message_scroll_offset: 0,
            config,
            client,
            allowlist,
            executor,
            session,
            approval_queue: VecDeque::new(),
            conversation,
            history,
            plan_receiver: None,
        }
    }

    pub fn submit_prompt(&mut self) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() {
            warn!("Attempted to submit empty prompt");
            return;
        }
        if self.plan_receiver.is_some() || self.is_loading_plan {
            warn!("Plan request already in progress - ignoring new prompt");
            self.add_message(
                "A plan is already running. Please wait for it to finish.".to_string(),
                MessageType::Warning,
            );
            self.log("A plan is already running. Please wait for it to finish.");
            return;
        }
        info!("Submitting prompt: {}", prompt);
        // Clear input immediately so user can see it's been submitted
        self.input.clear();

        // Set loading state - spinner will show until plan is received
        self.is_loading_plan = true;
        self.spinner_frame = 0;

        self.add_message(
            format!("Requesting plan for: {}", prompt),
            MessageType::Info,
        );
        self.log(format!("Requesting plan for: {}", prompt));

        // Store prompt for synthesis detection
        self.last_prompt = Some(prompt.clone());
        self.analysis_result = None; // Clear previous analysis
        self.analysis_scroll_offset = 0; // Reset scroll

        // Load conversation history
        let history = self.conversation.load_history().unwrap_or_else(|e| {
            warn!("Failed to load conversation history: {}", e);
            vec![]
        });
        debug!("Loaded {} conversation history entries", history.len());

        // Log prompt to conversation
        let _ = self.conversation.log(ConversationEntry::Prompt {
            timestamp: Utc::now().to_rfc3339(),
            prompt: prompt.clone(),
        });

        // Spawn background thread to fetch plan so UI can continue animating spinner
        let (tx, rx) = mpsc::channel();
        self.plan_receiver = Some(rx);
        let client = self.client.clone();
        let history_clone = history.clone();
        thread::spawn(move || {
            trace!("Background thread: calling API client.plan()");
            let result = client.plan(&prompt, &history_clone);
            let message = match result {
                Ok(response_text) => PlanResponse::Success(response_text),
                Err(err) => {
                    let formatted = format_error_chain(&err);
                    error!("Plan request failed in background thread: {}", formatted);
                    PlanResponse::Error(formatted)
                }
            };
            if tx.send(message).is_err() {
                warn!("Failed to send plan response back to main thread");
            }
        });
    }

    pub fn poll_plan_response(&mut self) {
        let Some(rx) = self.plan_receiver.take() else {
            return;
        };

        match rx.try_recv() {
            Ok(PlanResponse::Success(response_text)) => {
                self.is_loading_plan = false;
                self.handle_plan_response(response_text);
            }
            Ok(PlanResponse::Error(err_msg)) => {
                self.is_loading_plan = false;
                error!("Failed requesting plan: {}", err_msg);
                self.add_message(
                    format!("Failed requesting plan: {}", err_msg),
                    MessageType::Error,
                );
                self.log(format!("Failed requesting plan: {}", err_msg));
            }
            Err(TryRecvError::Empty) => {
                // No response yet - store receiver for future polling
                self.plan_receiver = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                self.is_loading_plan = false;
                warn!("Plan request channel disconnected before response received");
                self.log("Plan request channel disconnected before response finished.");
            }
        }
    }

    fn handle_plan_response(&mut self, response_text: String) {
        info!("Received plan response ({} bytes)", response_text.len());
        trace!(
            "Response preview: {}",
            response_text.chars().take(200).collect::<String>()
        );

        trace!("Parsing plan JSON");
        match parser::parse_plan(&response_text, &self.config.default_shell) {
            Ok(parsed) => {
                info!("Plan parsed successfully: {} tasks", parsed.tasks.len());
                self.summary = parsed.summary.clone();
                self.tasks = parsed.tasks.clone();
                self.selected = 0;

                // Log plan to conversation (include full response for context)
                let _ = self.conversation.log(ConversationEntry::Plan {
                    timestamp: Utc::now().to_rfc3339(),
                    summary: parsed.summary.clone(),
                    task_count: parsed.tasks.len(),
                    response: Some(response_text.clone()),
                });

                info!("Evaluating {} tasks against allowlist", self.tasks.len());
                let mut blocked_count = 0;
                for (idx, task) in self.tasks.iter_mut().enumerate() {
                    trace!("Evaluating task {}: {}", idx, task.description);
                    match self.allowlist.evaluate(task) {
                        Ok(status) => {
                            debug!("Task {} status: {:?}", idx, status);
                            task.status = status;
                        }
                        Err(err) => {
                            debug!("Task {} blocked: {}", idx, err);
                            task.status = TaskStatus::Blocked(err.to_string());
                            blocked_count += 1;
                        }
                    }
                }
                if blocked_count > 0 {
                    trace!("{} task(s) blocked by allowlist", blocked_count);
                }

                // Auto-complete Note tasks immediately and remove them from the list
                let mut notes_to_remove = Vec::new();
                for (idx, task) in self.tasks.iter_mut().enumerate() {
                    if matches!(task.detail, TaskDetail::Note { .. })
                        && matches!(task.status, TaskStatus::Ready | TaskStatus::Proposed)
                    {
                        info!("Auto-completing note task: {}", task.description);

                        if let TaskDetail::Note { ref details } = task.detail {
                            let _ = self.conversation.log(ConversationEntry::Note {
                                timestamp: Utc::now().to_rfc3339(),
                                task_id: task.id.clone(),
                                description: task.description.clone(),
                                details: details.clone(),
                            });
                        }

                        notes_to_remove.push(idx);
                    }
                }

                for &idx in notes_to_remove.iter().rev() {
                    self.tasks.remove(idx);
                    if self.selected >= idx && self.selected > 0 {
                        self.selected -= 1;
                    }
                }

                self.sort_tasks_by_status();

                trace!("Persisting plan");
                self.persist_plan();

                if let Some(ref summary) = self.summary {
                    self.add_message(summary.clone(), MessageType::Info);
                }
                self.add_message(
                    format!("Plan created with {} tasks", self.tasks.len()),
                    MessageType::Success,
                );
                self.log("Plan created successfully.");

                self.start_sequential_execution();
            }
            Err(err) => {
                let formatted = format_error_chain(&err);
                error!("Failed parsing plan: {}", formatted);
                self.add_message(
                    format!("Failed parsing plan: {}", formatted),
                    MessageType::Error,
                );
                self.log(format!("Failed parsing plan: {}", formatted));
            }
        }
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

    /// Add a message to the message stream (for TUI display)
    pub fn add_message(&mut self, content: String, msg_type: MessageType) {
        self.messages.push(Message {
            content,
            msg_type,
        });
        // Auto-scroll to bottom when new message arrives
        self.message_scroll_offset = 0;
    }

    /// Get all messages (used by TUI for rendering)
    pub fn get_all_messages(&self) -> &[Message] {
        &self.messages
    }

    /// Scroll messages up (show older messages)
    pub fn scroll_messages_up(&mut self) {
        let max_scroll = self.messages.len().saturating_sub(1);
        if self.message_scroll_offset < max_scroll {
            self.message_scroll_offset += 1;
        }
    }

    /// Scroll messages down (show newer messages)
    pub fn scroll_messages_down(&mut self) {
        if self.message_scroll_offset > 0 {
            self.message_scroll_offset -= 1;
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
                warn!(
                    "Task {} not ready for execution (status: {:?})",
                    self.selected, task.status
                );
                return;
            }
            info!("Executing task: {}", desc);
            task.status = TaskStatus::Running;
            // Reset spinner frame for this task's execution
            self.spinner_frame = 0;
            (task.detail.clone(), desc)
        };

        let task_id = self
            .tasks
            .get(self.selected)
            .map(|t| t.id.clone())
            .unwrap_or_else(|| "unknown".to_string());

        match detail {
            TaskDetail::Command(cmd) => {
                info!("Running command: {} (shell: {})", cmd.command, cmd.shell);
                trace!(
                    "Command details: cwd={:?}, requires_root={}",
                    cmd.cwd, cmd.requires_root
                );

                // Show command about to run
                let full_command = if let Some(ref cwd) = cmd.cwd {
                    format!("cd {} && {}", cwd, cmd.command)
                } else {
                    cmd.command.clone()
                };
                self.add_message(
                    format!("Running: {}", full_command),
                    MessageType::Command,
                );

                match self.executor.run_command(&cmd) {
                    Ok(result) => {
                        info!(
                            "Command executed successfully: exit_code={}, stdout_len={}, stderr_len={}",
                            result.status,
                            result.stdout.len(),
                            result.stderr.len()
                        );

                        // Write to history file
                        let _ = self.history.append_command(
                            &cmd.command,
                            cmd.cwd.as_deref(),
                            &result.stdout,
                            &result.stderr,
                        );

                        // Store result for display
                        self.execution_results.insert(self.selected, result.clone());

                        // Log to conversation
                        let _ = self.conversation.log(ConversationEntry::Command {
                            timestamp: Utc::now().to_rfc3339(),
                            task_id: task_id.clone(),
                            description: description.clone(),
                            command: cmd.command.clone(),
                            shell: cmd.shell.clone(),
                            exit_code: result.status,
                            stdout: result.stdout.clone(),
                            stderr: result.stderr.clone(),
                        });

                        // Show result
                        if result.status == 0 {
                            self.add_message(
                                format!("✓ Command succeeded (exit {})", result.status),
                                MessageType::Success,
                            );
                            if !result.stdout.trim().is_empty() {
                                let preview = if result.stdout.len() > 200 {
                                    format!("{}...", &result.stdout[..200])
                                } else {
                                    result.stdout.clone()
                                };
                                self.add_message(
                                    format!("Output: {}", preview),
                                    MessageType::Info,
                                );
                            }
                        } else {
                            self.add_message(
                                format!("✗ Command failed (exit {})", result.status),
                                MessageType::Error,
                            );
                            if !result.stderr.trim().is_empty() {
                                let preview = if result.stderr.len() > 200 {
                                    format!("{}...", &result.stderr[..200])
                                } else {
                                    result.stderr.clone()
                                };
                                self.add_message(
                                    format!("Error: {}", preview),
                                    MessageType::Error,
                                );
                            }
                        }

                        self.mark_complete_with_log(
                            format!("Executed '{}' exit {}", description, result.status),
                            Some(result),
                            None,
                        );

                        // After execution, continue to next task in sequence
                        self.continue_sequential_execution();
                    }
                    Err(err) => {
                        let formatted = format_error_chain(&err);
                        error!("Command execution failed: {}", formatted);
                        self.add_message(
                            format!("✗ Execution failed: {}", formatted),
                            MessageType::Error,
                        );
                        self.log(format!("Execution failed: {}", formatted));
                        self.set_blocked(format!("execution failed: {}", formatted));
                    }
                }
            }
            TaskDetail::FileEdit(edit) => {
                let path_str = edit.path.as_deref().unwrap_or("<no path>");
                info!(
                    "Applying file edit: {} ({} bytes)",
                    path_str,
                    edit.new_text.len()
                );
                match self.executor.apply_file_edit(&edit) {
                    Ok(outcome) => {
                        info!("File edit successful: {}", outcome.path.display());
                        if let Some(ref backup) = outcome.backup_path {
                            info!("Backup created: {}", backup.display());
                        }

                        // Log to conversation
                        let _ = self.conversation.log(ConversationEntry::FileEdit {
                            timestamp: Utc::now().to_rfc3339(),
                            task_id: task_id.clone(),
                            description: description.clone(),
                            path: outcome.path.display().to_string(),
                            backup_path: outcome
                                .backup_path
                                .as_ref()
                                .map(|p| p.display().to_string()),
                        });

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

                        // After execution, continue to next task in sequence
                        self.continue_sequential_execution();
                    }
                    Err(err) => {
                        let formatted = format_error_chain(&err);
                        error!("File edit failed: {}", formatted);
                        self.log(format!("Edit failed: {}", formatted));
                        self.set_blocked(format!("edit failed: {}", formatted));
                    }
                }
            }
            TaskDetail::Note { details } => {
                info!("Processing note task: {}", details);

                // Log to conversation
                let _ = self.conversation.log(ConversationEntry::Note {
                    timestamp: Utc::now().to_rfc3339(),
                    task_id: task_id.clone(),
                    description: description.clone(),
                    details: details.clone(),
                });

                self.log(format!("Note: {}", details));
                // Store selected task ID before status change
                let selected_task_id = self.tasks.get(self.selected).map(|t| t.id.clone());

                if let Some(task) = self.tasks.get_mut(self.selected) {
                    task.status = TaskStatus::Complete;
                }

                // Maintain task order (tasks stay in place when completed)
                self.sort_tasks_by_status();

                // Move selection to next incomplete task for linear progression
                let current_idx = if let Some(task_id) = selected_task_id {
                    self.tasks
                        .iter()
                        .position(|t| t.id == task_id)
                        .unwrap_or(self.selected)
                } else {
                    self.selected
                };

                let next_incomplete = self
                    .tasks
                    .iter()
                    .enumerate()
                    .skip(current_idx + 1)
                    .find(|(_, t)| !matches!(t.status, TaskStatus::Complete));

                if let Some((idx, _)) = next_incomplete {
                    self.selected = idx;
                } else {
                    self.select_first_incomplete_or_blocked();
                }
            }
        }
    }

    fn mark_complete_with_log(
        &mut self,
        summary: String,
        exec: Option<ExecutionResult>,
        edit: Option<FileEditOutcome>,
    ) {
        // Store selected task ID before status change
        let selected_task_id = self.tasks.get(self.selected).map(|t| t.id.clone());

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

        // Maintain task order (tasks stay in place when completed)
        self.sort_tasks_by_status();

        // Restore selection to the completed task (it stays in place, just marked complete)
        // continue_sequential_execution() will handle moving to the next task
        if let Some(task_id) = selected_task_id
            && let Some(new_idx) = self.tasks.iter().position(|t| t.id == task_id) {
                self.selected = new_idx;
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


    /// Select the first incomplete task in order, prioritizing ready tasks over blocked
    /// For sequential execution, we want ready tasks to run first, then prompt for blocked ones
    fn select_first_incomplete_or_blocked(&mut self) {
        if self.tasks.is_empty() {
            self.selected = 0;
            return;
        }

        // First, look for ready tasks (they can run immediately)
        for (idx, task) in self.tasks.iter().enumerate() {
            if matches!(task.status, TaskStatus::Ready) {
                self.selected = idx;
                return;
            }
        }

        // Then look for blocked tasks (they need approval)
        for (idx, task) in self.tasks.iter().enumerate() {
            if matches!(task.status, TaskStatus::Blocked(_)) {
                self.selected = idx;
                return;
            }
        }

        // Then find any other incomplete task
        for (idx, task) in self.tasks.iter().enumerate() {
            if !matches!(task.status, TaskStatus::Complete) {
                self.selected = idx;
                return;
            }
        }

        // If all complete, select first task (index 0)
        self.selected = 0;
    }

    /// Check if prompt requests analysis/synthesis and trigger if needed
    fn check_and_synthesize_results(&mut self) {
        // Synthesize if:
        // 1. All executable tasks are complete
        // 2. We have execution results to analyze
        // 3. We haven't already synthesized

        // Check if all executable tasks are complete
        let has_executable_tasks = self
            .tasks
            .iter()
            .any(|t| matches!(t.detail, TaskDetail::Command(_) | TaskDetail::FileEdit(_)));

        if !has_executable_tasks {
            debug!("No executable tasks to synthesize");
            return;
        }

        let all_complete = self.tasks.iter().all(|t| {
            matches!(t.status, TaskStatus::Complete) || matches!(t.detail, TaskDetail::Note { .. })
        });

        if !all_complete {
            debug!("Not all tasks complete yet, waiting");
            return;
        }

        // Check if we already synthesized
        if self.analysis_result.is_some() {
            debug!("Already synthesized results");
            return;
        }

        // Check if we have any execution results
        if self.execution_results.is_empty() {
            debug!("No execution results to synthesize");
            self.log("All tasks completed.");
            return;
        }

        // Always synthesize when all tasks complete and we have results
        info!("Triggering synthesis after all tasks completed");
        self.add_message(
            "All tasks completed. Generating analysis...".to_string(),
            MessageType::Info,
        );
        self.synthesize_results();
    }


    /// Synthesize execution results into an analysis
    fn synthesize_results(&mut self) {
        info!("Synthesizing execution results");

        // Collect all execution results
        let mut results_summary = String::new();
        results_summary.push_str("Execution Results:\n\n");

        for (idx, task) in self.tasks.iter().enumerate() {
            if matches!(
                task.detail,
                TaskDetail::Command(_) | TaskDetail::FileEdit(_)
            ) {
                results_summary.push_str(&format!("Task {}: {}\n", idx + 1, task.description));

                if let Some(exec_result) = self.execution_results.get(&idx) {
                    results_summary.push_str(&format!("  Exit code: {}\n", exec_result.status));
                    if !exec_result.stdout.trim().is_empty() {
                        results_summary.push_str(&format!("  STDOUT:\n{}\n", exec_result.stdout));
                    }
                    if !exec_result.stderr.trim().is_empty() {
                        results_summary.push_str(&format!("  STDERR:\n{}\n", exec_result.stderr));
                    }
                }

                if let TaskDetail::FileEdit(_) = task.detail {
                    results_summary.push_str("  File edit completed\n");
                }

                results_summary.push('\n');
            }
        }

        // Build synthesis prompt
        let original_prompt = self.last_prompt.as_deref().unwrap_or("Analyze the results");
        let synthesis_prompt = format!("{}\n\n{}", original_prompt, results_summary);

        // Load conversation history
        let history = self.conversation.load_history().unwrap_or_else(|e| {
            warn!("Failed to load conversation history: {}", e);
            vec![]
        });

        // Request synthesis (use a different system prompt for analysis)
        match self.client.synthesize(&synthesis_prompt, &history) {
            Ok(analysis) => {
                info!("Received synthesis result ({} chars)", analysis.len());
                self.analysis_result = Some(analysis.clone());
                self.analysis_scroll_offset = 0; // Reset scroll when new analysis arrives
                
                self.add_message(
                    "✓ Analysis complete".to_string(),
                    MessageType::Success,
                );
                self.add_message(analysis.clone(), MessageType::Recommendation);
                self.log("✓ Analysis complete. Review in Results pane (↑/↓ to scroll).");
                self.log("Next: Ask a follow-up question or press 'r' to run more tasks.");

                // Log analysis to conversation
                let _ = self.conversation.log(ConversationEntry::Note {
                    timestamp: Utc::now().to_rfc3339(),
                    task_id: "synthesis".to_string(),
                    description: "Analysis Result".to_string(),
                    details: analysis,
                });
            }
            Err(err) => {
                let formatted = format_error_chain(&err);
                error!("Synthesis failed: {}", formatted);
                self.add_message(
                    "All tasks completed successfully. (Synthesis unavailable)".to_string(),
                    MessageType::Warning,
                );
                self.log("All tasks completed successfully. (Synthesis unavailable)");
            }
        }
    }



    /// Maintain tasks in original order - don't reorder by status
    /// This preserves the linear flow of the plan as tasks are completed
    fn sort_tasks_by_status(&mut self) {
        // Keep tasks in their original order (by created_at)
        // This maintains the linear progression of the plan
        // Completed tasks stay in place, just marked as complete
        self.tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    }

    /// Start sequential execution: check first task in order and either run it or wait for approval
    fn start_sequential_execution(&mut self) {
        if let Some(idx) = self.first_pending_index() {
            self.selected = idx;
            let description = self.tasks[idx].description.clone();
            match self.tasks[idx].status.clone() {
                TaskStatus::Ready | TaskStatus::Proposed => {
                    self.add_message(
                        format!("Starting plan execution with: {}", description),
                        MessageType::Info,
                    );
                    self.log(format!("Starting plan execution with: {}", description));
                    self.execute_selected();
                }
                TaskStatus::Blocked(_) => {
                    self.approval_queue.clear();
                    self.approval_queue.push_back(idx);
                    self.log(format!(
                        "First task requires approval before running: {}",
                        description
                    ));
                }
                TaskStatus::Running => {
                    self.log(format!("Waiting for running task: {}", description));
                }
                TaskStatus::Complete => {
                    // Should not happen, but fall back to continue logic
                    self.continue_sequential_execution();
                }
            }
        } else {
            self.log("All tasks complete.");
            self.check_and_synthesize_results();
        }
    }

    /// Continue sequential execution: after a task completes, move to next and execute
    fn continue_sequential_execution(&mut self) {
        // Check if we should synthesize first
        self.check_and_synthesize_results();

        if let Some(idx) = self.first_pending_index() {
            self.selected = idx;
            let description = self.tasks[idx].description.clone();
            match self.tasks[idx].status.clone() {
                TaskStatus::Ready | TaskStatus::Proposed => {
                    self.add_message(
                        format!("Continuing with: {}", description),
                        MessageType::Info,
                    );
                    self.log(format!("Continuing with: {}", description));
                    self.execute_selected();
                }
                TaskStatus::Blocked(_) => {
                    self.approval_queue.clear();
                    self.approval_queue.push_back(idx);
                    self.log(format!("Next task requires approval: {}", description));
                }
                TaskStatus::Running => {
                    self.log(format!("Waiting for running task: {}", description));
                }
                TaskStatus::Complete => {
                    // Should not happen, but try again on next tick
                }
            }
        } else {
            // No more incomplete tasks
            self.log("All tasks complete.");
            self.check_and_synthesize_results();
        }
    }

    fn first_pending_index(&self) -> Option<usize> {
        self.tasks
            .iter()
            .enumerate()
            .find(|(_, t)| !matches!(t.status, TaskStatus::Complete))
            .map(|(idx, _)| idx)
    }
}

fn format_error_chain(err: &Error) -> String {
    let mut parts = Vec::new();
    for cause in err.chain() {
        let cleaned = cause
            .to_string()
            .replace(['\n', '\r'], " ")
            .trim()
            .to_string();
        if !cleaned.is_empty() {
            parts.push(cleaned);
        }
    }
    if parts.is_empty() {
        "Unknown error".to_string()
    } else {
        parts
            .into_iter()
            .enumerate()
            .map(|(idx, part)| {
                if idx == 0 {
                    part
                } else {
                    format!("caused by: {}", part)
                }
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

fn truncate(text: &str) -> String {
    const LIMIT: usize = 200;
    if text.chars().count() <= LIMIT {
        text.to_string()
    } else {
        // Use char_indices to safely truncate at character boundaries
        let mut truncated = String::with_capacity(LIMIT + 1);
        for (_idx, ch) in text.char_indices() {
            if truncated.chars().count() >= LIMIT {
                break;
            }
            truncated.push(ch);
        }
        format!("{}…", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::executor::Executor;
    use crate::session::SessionStore;
    use tempfile::TempDir;

    fn create_test_app() -> App {
        // Set a dummy API key for tests if not already set
        if std::env::var("ANTHROPIC_API_KEY").is_err() {
            unsafe {
                std::env::set_var("ANTHROPIC_API_KEY", "sk-test-dummy-key-for-testing");
            }
        }
        
        // Try to load config, but use offline mode for tests
        let mut config = AppConfig::load().unwrap_or_else(|e| {
            panic!("Cannot create test app without config: {}. Set ANTHROPIC_API_KEY environment variable or create config file.", e);
        });
        config.offline_mode = true; // Force offline mode for tests
        let client = AnthropicClient::new(&config).unwrap();
        let allowlist = Allowlist::from_config(config.allowlist.clone()).unwrap();
        let executor = Executor::new(false);
        let session_dir = TempDir::new().unwrap();
        let session = SessionStore::new(session_dir.path().to_path_buf()).unwrap();
        App::new(config, client, allowlist, executor, session)
    }

    #[test]
    fn add_message_appends_to_stream() {
        let mut app = create_test_app();
        assert_eq!(app.messages.len(), 0);
        
        app.add_message("Test message".to_string(), MessageType::Info);
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].content, "Test message");
    }

    #[test]
    fn get_all_messages_returns_all() {
        let mut app = create_test_app();
        
        // Add several messages
        for i in 0..5 {
            app.add_message(format!("Message {}", i), MessageType::Info);
        }
        
        // Should get all messages
        let all = app.get_all_messages();
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn scroll_messages_up_increases_offset() {
        let mut app = create_test_app();
        
        for i in 0..5 {
            app.add_message(format!("Message {}", i), MessageType::Info);
        }
        
        assert_eq!(app.message_scroll_offset, 0);
        app.scroll_messages_up();
        assert_eq!(app.message_scroll_offset, 1);
    }

    #[test]
    fn scroll_messages_down_decreases_offset() {
        let mut app = create_test_app();
        
        for i in 0..5 {
            app.add_message(format!("Message {}", i), MessageType::Info);
        }
        
        app.message_scroll_offset = 2;
        app.scroll_messages_down();
        assert_eq!(app.message_scroll_offset, 1);
    }

    #[test]
    fn scroll_messages_down_does_not_go_below_zero() {
        let mut app = create_test_app();
        
        app.message_scroll_offset = 0;
        app.scroll_messages_down();
        assert_eq!(app.message_scroll_offset, 0);
    }

    #[test]
    fn new_message_resets_scroll_offset() {
        let mut app = create_test_app();
        
        app.message_scroll_offset = 5;
        app.add_message("New message".to_string(), MessageType::Info);
        assert_eq!(app.message_scroll_offset, 0);
    }
}
