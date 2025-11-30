use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use chrono::Utc;
use log::{debug, error, info, trace, warn};

use crate::allowlist::Allowlist;
use crate::api::AnthropicClient;
use crate::config::AppConfig;
use crate::conversation::{ConversationEntry, ConversationLogger};
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
    pub execution_results: HashMap<usize, ExecutionResult>, // task index -> execution result
    pub analysis_result: Option<String>, // Synthesis/analysis result from LLM
    pub analysis_scroll_offset: usize, // Scroll offset for analysis display
    last_prompt: Option<String>, // Store last prompt for synthesis detection
    config: AppConfig,
    client: AnthropicClient,
    allowlist: Allowlist,
    executor: Executor,
    session: SessionStore,
    approval_queue: VecDeque<usize>,
    conversation: ConversationLogger,
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
        
        // Initialize conversation logger
        let conversation_path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("sysaidmin.conversation.jsonl");
        let conversation = ConversationLogger::new(conversation_path.clone())
            .unwrap_or_else(|e| {
                warn!("Failed to create conversation logger: {}", e);
                // Create a dummy logger that does nothing
                ConversationLogger::new(PathBuf::from("/dev/null"))
                    .expect("Failed to create dummy conversation logger")
            });
        info!("Conversation logger initialized at: {}", conversation_path.display());
        
        Self {
            tasks: Vec::new(),
            selected: 0,
            input: String::new(),
            input_mode: InputMode::Prompt,
            logs: Vec::new(),
            summary: None,
            execution_results: HashMap::new(),
            analysis_result: None,
            analysis_scroll_offset: 0,
            last_prompt: None,
            config,
            client,
            allowlist,
            executor,
            session,
            approval_queue: VecDeque::new(),
            conversation,
        }
    }

    pub fn submit_prompt(&mut self) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() {
            warn!("Attempted to submit empty prompt");
            return;
        }
        info!("Submitting prompt: {}", prompt);
        // Clear input immediately so user can see it's been submitted
        self.input.clear();
        
        self.log(format!("Requesting plan for: {}", prompt));
        
        // Store prompt for synthesis detection
        self.last_prompt = Some(prompt.clone());
        self.analysis_result = None; // Clear previous analysis
        self.analysis_scroll_offset = 0; // Reset scroll
        
        // Load conversation history
        let history = self.conversation.load_history()
            .unwrap_or_else(|e| {
                warn!("Failed to load conversation history: {}", e);
                vec![]
            });
        debug!("Loaded {} conversation history entries", history.len());
        
        // Log prompt to conversation
        let _ = self.conversation.log(ConversationEntry::Prompt {
            timestamp: Utc::now().to_rfc3339(),
            prompt: prompt.clone(),
        });
        
        trace!("Calling API client.plan() with conversation history");
        match self.client.plan(&prompt, &history) {
            Ok(response_text) => {
                info!("Received plan response ({} bytes)", response_text.len());
                trace!("Response preview: {}", response_text.chars().take(200).collect::<String>());
                
                trace!("Parsing plan JSON");
                match parser::parse_plan(&response_text, &self.config.default_shell) {
                    Ok(parsed) => {
                        info!("Plan parsed successfully: {} tasks", parsed.tasks.len());
                        self.summary = parsed.summary.clone();
                        self.tasks = parsed.tasks.clone();
                        // Input already cleared when prompt was submitted
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
                        // Log summary instead of individual task blocks to reduce noise
                        if blocked_count > 0 {
                            trace!("{} task(s) blocked by allowlist", blocked_count);
                        }
                        
                        // Auto-complete Note tasks immediately and remove them from the list
                        // They're informational and clutter the display once completed
                        let mut notes_to_remove = Vec::new();
                        for (idx, task) in self.tasks.iter_mut().enumerate() {
                            if matches!(task.detail, TaskDetail::Note { .. }) {
                                if matches!(task.status, TaskStatus::Ready | TaskStatus::Proposed) {
                                    info!("Auto-completing note task: {}", task.description);
                                    
                                    // Log note to conversation immediately
                                    if let TaskDetail::Note { ref details } = task.detail {
                                        let _ = self.conversation.log(ConversationEntry::Note {
                                            timestamp: Utc::now().to_rfc3339(),
                                            task_id: task.id.clone(),
                                            description: task.description.clone(),
                                            details: details.clone(),
                                        });
                                    }
                                    
                                    // Mark for removal (remove in reverse order to preserve indices)
                                    notes_to_remove.push(idx);
                                }
                            }
                        }
                        
                        // Remove completed Note tasks in reverse order
                        for &idx in notes_to_remove.iter().rev() {
                            self.tasks.remove(idx);
                            // Adjust selection if needed
                            if self.selected >= idx && self.selected > 0 {
                                self.selected -= 1;
                            }
                        }
                        
                        // Maintain tasks in original order for linear plan progression
                        self.sort_tasks_by_status();
                        
                        trace!("Persisting plan");
                        self.persist_plan();
                        
                        trace!("Enqueueing blocked tasks");
                        self.enqueue_blocked();
                        
                        self.log("Plan created successfully.");
                        
                        // Select first task in the plan (in order)
                        self.select_first_incomplete_or_blocked();
                        
                        // Start sequential execution: if first task is ready, run it; if blocked, wait for approval
                        self.start_sequential_execution();
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

    pub fn notify(&mut self, message: impl Into<String>) {
        self.log(message);
    }

    pub fn scroll_analysis_up(&mut self) {
        if self.analysis_scroll_offset > 0 {
            self.analysis_scroll_offset -= 1;
        }
    }

    pub fn scroll_analysis_down(&mut self) {
        if self.analysis_result.is_some() {
            self.analysis_scroll_offset = self.analysis_scroll_offset.saturating_add(1);
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

        let task_id = self.tasks.get(self.selected)
            .map(|t| t.id.clone())
            .unwrap_or_else(|| "unknown".to_string());
        
        match detail {
            TaskDetail::Command(cmd) => {
                info!("Running command: {} (shell: {})", cmd.command, cmd.shell);
                trace!("Command details: cwd={:?}, requires_root={}", cmd.cwd, cmd.requires_root);
                match self.executor.run_command(&cmd) {
                    Ok(result) => {
                        info!("Command executed successfully: exit_code={}, stdout_len={}, stderr_len={}", 
                              result.status, result.stdout.len(), result.stderr.len());
                        
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
                        
                        self.mark_complete_with_log(
                            format!("Executed '{}' exit {}", description, result.status),
                            Some(result),
                            None,
                        );
                        
                        // After execution, continue to next task in sequence
                        self.continue_sequential_execution();
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
                        
                        // Log to conversation
                        let _ = self.conversation.log(ConversationEntry::FileEdit {
                            timestamp: Utc::now().to_rfc3339(),
                            task_id: task_id.clone(),
                            description: description.clone(),
                            path: outcome.path.display().to_string(),
                            backup_path: outcome.backup_path.as_ref()
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
                        error!("File edit failed: {:?}", err);
                        self.log(format!("Edit failed: {err:?}"));
                        self.set_blocked(format!("edit failed: {err}"));
                    }
                }
                return;
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
                    self.tasks.iter().position(|t| t.id == task_id).unwrap_or(self.selected)
                } else {
                    self.selected
                };
                
                let next_incomplete = self.tasks.iter()
                    .enumerate()
                    .skip(current_idx + 1)
                    .find(|(_, t)| !matches!(t.status, TaskStatus::Complete));
                
                if let Some((idx, _)) = next_incomplete {
                    self.selected = idx;
                } else {
                    self.select_first_incomplete_or_blocked();
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
        
        // Move selection to next incomplete task for linear progression
        // Find next task after the completed one that's not complete
        let current_idx = if let Some(task_id) = selected_task_id {
            self.tasks.iter().position(|t| t.id == task_id).unwrap_or(self.selected)
        } else {
            self.selected
        };
        
        // Look for next incomplete task starting from current position
        let next_incomplete = self.tasks.iter()
            .enumerate()
            .skip(current_idx + 1)
            .find(|(_, t)| !matches!(t.status, TaskStatus::Complete));
        
        if let Some((idx, _)) = next_incomplete {
            self.selected = idx;
        } else {
            // No incomplete tasks after this one, stay on current or move to first incomplete
            self.select_first_incomplete_or_blocked();
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

    #[allow(dead_code)] // Public API method, may be used by TUI or external code
    pub fn dry_run(&self) -> bool {
        self.config.dry_run
    }

    pub fn run_ready_tasks(&mut self) {
        // Collect all ready task IDs first (before sorting changes indices)
        let ready_task_ids: Vec<String> = self
            .tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::Ready))
            .map(|t| t.id.clone())
            .collect();
        
        if ready_task_ids.is_empty() {
            let blocked_count = self.tasks.iter().filter(|t| matches!(t.status, TaskStatus::Blocked(_))).count();
            if blocked_count > 0 {
                self.notify(format!("No ready tasks. {} blocked task(s) need approval (y/n).", blocked_count));
            } else {
                self.notify("All tasks complete. Ask a new question or review results.");
            }
            return;
        }
        
        self.log(format!("Running {} ready task(s)...", ready_task_ids.len()));
        
        // Execute each ready task by ID (so we find it even after sorting)
        for task_id in ready_task_ids {
            if let Some(idx) = self.tasks.iter().position(|t| t.id == task_id) {
                self.selected = idx;
                let description = self.tasks[idx].description.clone();
                self.log(format!("▶ Executing: {}", description));
                self.execute_selected();
            }
        }
        
        // After running tasks, reset selection to first incomplete/blocked task
        self.select_first_incomplete_or_blocked();
        // After running tasks, check if we should synthesize
        self.check_and_synthesize_results();
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
        let has_executable_tasks = self.tasks.iter().any(|t| {
            matches!(t.detail, TaskDetail::Command(_) | TaskDetail::FileEdit(_))
        });
        
        if !has_executable_tasks {
            debug!("No executable tasks to synthesize");
            return;
        }
        
        let all_complete = self.tasks.iter().all(|t| {
            matches!(t.status, TaskStatus::Complete) || 
            matches!(t.detail, TaskDetail::Note { .. })
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
        self.synthesize_results();
    }

    /// Detect if a prompt requests analysis/synthesis
    #[allow(dead_code)] // May be used for future fine-tuning
    fn prompt_needs_synthesis(&self, prompt: &str) -> bool {
        let lower = prompt.to_lowercase();
        // Keywords that indicate analysis is needed
        let analysis_keywords = [
            "analyze", "analysis", "examine", "investigate", "review",
            "what is", "describe", "explain", "summarize", "synthesis",
            "tell me about", "show me", "what are", "identify",
        ];
        
        analysis_keywords.iter().any(|keyword| lower.contains(keyword))
    }

    /// Synthesize execution results into an analysis
    fn synthesize_results(&mut self) {
        info!("Synthesizing execution results");
        self.log("✓ All tasks completed. Generating analysis...");
        
        // Collect all execution results
        let mut results_summary = String::new();
        results_summary.push_str("Execution Results:\n\n");
        
        for (idx, task) in self.tasks.iter().enumerate() {
            if matches!(task.detail, TaskDetail::Command(_) | TaskDetail::FileEdit(_)) {
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
                
                results_summary.push_str("\n");
            }
        }
        
        // Build synthesis prompt
        let original_prompt = self.last_prompt.as_deref().unwrap_or("Analyze the results");
        let synthesis_prompt = format!(
            "{}\n\n{}",
            original_prompt,
            results_summary
        );
        
        // Load conversation history
        let history = self.conversation.load_history()
            .unwrap_or_else(|e| {
                warn!("Failed to load conversation history: {}", e);
                vec![]
            });
        
        // Request synthesis (use a different system prompt for analysis)
        match self.client.synthesize(&synthesis_prompt, &history) {
            Ok(analysis) => {
                info!("Received synthesis result ({} chars)", analysis.len());
                self.analysis_result = Some(analysis.clone());
                self.analysis_scroll_offset = 0; // Reset scroll when new analysis arrives
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
                error!("Synthesis failed: {:?}", err);
                self.log("All tasks completed successfully. (Synthesis unavailable)");
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
                    // Truncate reason to prevent overflow (max 100 chars)
                    let truncated_reason = if reason.len() > 100 {
                        format!("{}…", &reason[..100])
                    } else {
                        reason.clone()
                    };
                    Some(format!(
                        "Allow blocked task '{}'?\nReason: {}\nPress 'y' to allow, 'n' to skip.",
                        task.description, truncated_reason
                    ))
                } else {
                    None
                }
            })
    }

    pub fn approve_current_blocked(&mut self) {
        if let Some(idx) = self.approval_queue.pop_front() {
            if idx < self.tasks.len() {
                // Store selected task ID before status change
                let selected_task_id = self.tasks.get(idx).map(|t| t.id.clone());
                let description = self.tasks[idx].description.clone();

                self.selected = idx;
                if let Some(task) = self.tasks.get_mut(idx) {
                    task.status = TaskStatus::Ready;
                }
                self.log(format!("✓ Approved: '{}' (now ready to run)", description));
                
                // Maintain task order after status change
                self.sort_tasks_by_status();

                // Selection stays on the same task (it doesn't move)
                if let Some(task_id) = selected_task_id {
                    if let Some(new_idx) = self.tasks.iter().position(|t| t.id == task_id) {
                        self.selected = new_idx;
                    }
                }

                // Execute the newly approved task, then continue sequentially
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
            self.log(format!("✗ Skipped: '{}' (remains blocked)", message));
            
            // After rejecting, continue to next task in sequence
            self.continue_sequential_execution();
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

    /// Maintain tasks in original order - don't reorder by status
    /// This preserves the linear flow of the plan as tasks are completed
    fn sort_tasks_by_status(&mut self) {
        // Keep tasks in their original order (by created_at)
        // This maintains the linear progression of the plan
        // Completed tasks stay in place, just marked as complete
        self.tasks.sort_by(|a, b| {
            a.created_at.cmp(&b.created_at)
        });
    }

    /// Start sequential execution: check first task in order and either run it or wait for approval
    fn start_sequential_execution(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        
        // Find first incomplete task in order (prioritize ready over blocked for immediate execution)
        // First try to find a ready task (can run immediately)
        if let Some((idx, task)) = self.tasks.iter().enumerate().find(|(_, t)| matches!(t.status, TaskStatus::Ready)) {
            self.selected = idx;
            let description = task.description.clone();
            self.log(format!("Starting plan execution with: {}", description));
            self.execute_selected();
            return;
        }
        
        // If no ready tasks, find first blocked task (needs approval)
        if let Some((idx, task)) = self.tasks.iter().enumerate().find(|(_, t)| matches!(t.status, TaskStatus::Blocked(_))) {
            self.selected = idx;
            let description = task.description.clone();
            self.enqueue_blocked();
            self.log(format!("First task requires approval: {}", description));
            return;
        }
        
        // If no ready or blocked, find any incomplete task
        if let Some((idx, _)) = self.tasks.iter().enumerate().find(|(_, t)| !matches!(t.status, TaskStatus::Complete)) {
            self.selected = idx;
            self.log("Plan ready. Review tasks and proceed.");
        } else {
            // All tasks complete
            self.log("All tasks complete.");
            self.check_and_synthesize_results();
        }
    }

    /// Continue sequential execution: after a task completes, move to next and execute
    fn continue_sequential_execution(&mut self) {
        // Check if we should synthesize first
        self.check_and_synthesize_results();
        
        // Find next incomplete task after current position
        let current_idx = self.selected;
        let next_incomplete = self.tasks.iter()
            .enumerate()
            .skip(current_idx + 1)
            .find(|(_, t)| !matches!(t.status, TaskStatus::Complete));
        
        if let Some((idx, task)) = next_incomplete {
            self.selected = idx;
            let description = task.description.clone();
            
            match &task.status {
                TaskStatus::Ready => {
                    // Next task is ready - run it immediately
                    self.log(format!("Continuing with: {}", description));
                    self.execute_selected();
                }
                TaskStatus::Blocked(_) => {
                    // Next task is blocked - wait for approval
                    self.enqueue_blocked();
                    self.log(format!("Next task requires approval: {}", description));
                }
                _ => {
                    // Other statuses - just select it
                }
            }
        } else {
            // No more incomplete tasks
            self.log("All tasks complete.");
            self.check_and_synthesize_results();
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

