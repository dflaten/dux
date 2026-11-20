use anyhow::{Context, bail};
use std::os::unix::fs::PermissionsExt;

use super::*;

impl App {
    pub(crate) fn start_control_server(&mut self) {
        let (tx, rx) = mpsc::channel();
        match ControlServer::start(self.paths.control_socket_path.clone(), tx) {
            Ok(server) => {
                logger::info(&format!(
                    "agent control socket listening at {}",
                    server.path().display()
                ));
                self.control_server = Some(server);
                self.control_rx = Some(rx);
            }
            Err(err) => {
                logger::error(&format!("agent control socket unavailable: {err:#}"));
                self.set_warning(format!(
                    "Agent control is unavailable: {err:#}. Agents cannot create related worktrees until this is fixed."
                ));
            }
        }
    }

    pub(crate) fn drain_control_requests(&mut self) {
        let requests = match &self.control_rx {
            Some(rx) => rx.try_iter().collect::<Vec<_>>(),
            None => return,
        };
        for incoming in requests {
            self.handle_control_request(incoming);
        }
    }

    fn handle_control_request(&mut self, incoming: IncomingControlRequest) {
        let IncomingControlRequest { request, respond } = incoming;
        if matches!(
            &request.command,
            ControlCommand::DelegationStatus { .. } | ControlCommand::DelegationResult { .. }
        ) {
            self.import_pending_delegation_results();
        }
        if !self.config.agent_control.enabled {
            let _ = respond.send(ControlResponse::error(
                "disabled",
                "Agent control is disabled. Set [agent_control] enabled = true in config.toml and reload the config.",
            ));
            return;
        }

        match request.command {
            ControlCommand::Related { source_session_id } => {
                let response = self.related_agents_response(source_session_id.as_deref());
                let _ = respond.send(response);
            }
            ControlCommand::PullRequests { source_session_id } => {
                let response = self.pull_requests_response(source_session_id.as_deref());
                let _ = respond.send(response);
            }
            ControlCommand::LinkPullRequest {
                source_session_id,
                input,
            } => {
                self.handle_link_pull_request(source_session_id, input, respond);
            }
            ControlCommand::SplitOut(params) => {
                self.handle_split_out_request(params, respond, true);
            }
            ControlCommand::Delegate(params) => {
                self.handle_split_out_request(params, respond, false);
            }
            ControlCommand::DelegationStatus {
                source_session_id,
                session_id,
                delegation_id,
            } => {
                let response = self.delegation_status_response(
                    source_session_id.as_deref(),
                    session_id.as_deref(),
                    delegation_id.as_deref(),
                );
                let _ = respond.send(response);
            }
            ControlCommand::DelegationResult {
                source_session_id,
                session_id,
                delegation_id,
            } => {
                let response = self.delegation_result_response(
                    source_session_id.as_deref(),
                    session_id.as_deref(),
                    delegation_id.as_deref(),
                );
                let _ = respond.send(response);
            }
            ControlCommand::DelegationFinish {
                source_session_id,
                session_id,
                delegation_id,
                result,
                result_path,
            } => {
                let response = self.handle_delegation_finish(
                    source_session_id.as_deref(),
                    session_id.as_deref(),
                    delegation_id.as_deref(),
                    result,
                    result_path,
                );
                let _ = respond.send(response);
            }
            ControlCommand::DelegationFollowup {
                source_session_id,
                session_id,
                delegation_id,
                text,
                submit,
                message_only,
            } => {
                let response = self.handle_delegation_followup(
                    source_session_id.as_deref(),
                    session_id.as_deref(),
                    delegation_id.as_deref(),
                    &text,
                    submit,
                    message_only,
                );
                let _ = respond.send(response);
            }
            ControlCommand::DelegationCancel {
                source_session_id,
                session_id,
                delegation_id,
                subtree,
            } => {
                let response = self.handle_delegation_cancel(
                    source_session_id.as_deref(),
                    session_id.as_deref(),
                    delegation_id.as_deref(),
                    subtree,
                );
                let _ = respond.send(response);
            }
            ControlCommand::DelegationRetry { delegation_id } => {
                let response = self.handle_delegation_retry(&delegation_id);
                let _ = respond.send(response);
            }
            ControlCommand::DelegationIntegrate {
                source_session_id,
                session_id,
            } => {
                self.handle_delegation_integrate(source_session_id.as_deref(), session_id, respond);
            }
        }
    }

    fn handle_link_pull_request(
        &mut self,
        source_session_id: Option<String>,
        input: String,
        respond: Sender<ControlResponse>,
    ) {
        let Some(source_session_id) = source_session_id else {
            let _ = respond.send(ControlResponse::error(
                "invalid_source",
                "No source session was supplied. Set DUX_SESSION_ID or pass --session.",
            ));
            return;
        };
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == source_session_id)
            .cloned()
        else {
            let response = if self
                .archived_sessions
                .iter()
                .any(|session| session.id == source_session_id)
            {
                ControlResponse::error(
                    "invalid_source",
                    "That agent is archived. Restore it before linking a pull request.",
                )
            } else {
                ControlResponse::error(
                    "invalid_source",
                    format!("Agent session \"{source_session_id}\" no longer exists."),
                )
            };
            let _ = respond.send(response);
            return;
        };
        let Some(project) = self
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .cloned()
        else {
            let _ = respond.send(ControlResponse::error(
                "invalid_project",
                "The agent's project is no longer registered.",
            ));
            return;
        };
        let request_id = Uuid::new_v4().to_string();
        self.pending_control_responses
            .insert(request_id.clone(), respond);
        self.set_busy(format!(
            "Linking a pull request to agent \"{}\"...",
            session.branch_name
        ));
        let worker_tx = self.worker_tx.clone();
        thread::spawn(move || {
            super::workers::run_pr_link_job(request_id, session.id, project, input, worker_tx);
        });
    }

    pub(crate) fn handle_split_out_request(
        &mut self,
        params: SplitOutParams,
        respond: Sender<ControlResponse>,
        legacy_split_out: bool,
    ) {
        if !self.config.orchestration.enabled {
            let _ = respond.send(ControlResponse::error(
                "disabled",
                "Orchestration is disabled. Set [orchestration] enabled = true in config.toml.",
            ));
            return;
        }
        if let Some(mode) = params.base_mode.as_deref()
            && BaseMode::from_str(mode).is_none()
        {
            let _ = respond.send(ControlResponse::error(
                "invalid_base_mode",
                format!(
                    "Unknown base mode \"{mode}\". Use leading, ref, parent-head, parent-snapshot, or sibling-head."
                ),
            ));
            return;
        }
        if let Some(mode) = params.base_mode.as_deref().and_then(BaseMode::from_str) {
            if mode == BaseMode::Ref && params.base.is_none() {
                let _ = respond.send(ControlResponse::error(
                    "invalid_base",
                    "--base-mode ref requires --base <ref>.",
                ));
                return;
            }
            if mode == BaseMode::SiblingHead && params.base_session.is_none() {
                let _ = respond.send(ControlResponse::error(
                    "invalid_base",
                    "--base-mode sibling-head requires --base-session <id>.",
                ));
                return;
            }
            if mode == BaseMode::ParentSnapshot && !self.config.orchestration.allow_parent_snapshot
            {
                let _ = respond.send(ControlResponse::error(
                    "invalid_base",
                    "parent-snapshot is disabled by [orchestration] allow_parent_snapshot.",
                ));
                return;
            }
        }
        if let Some(provider) = params.provider.as_deref()
            && self.config.providers.get(provider).is_none()
        {
            let _ = respond.send(ControlResponse::error(
                "provider_not_configured",
                format!("Provider \"{provider}\" is not configured in config.toml."),
            ));
            return;
        }
        if let Some(name) = params.name.as_deref()
            && !git::is_valid_agent_name(name)
        {
            let _ = respond.send(ControlResponse::error(
                "invalid_name",
                format!(
                    "Invalid agent name \"{name}\". Use letters, numbers, dashes, underscores, and slashes."
                ),
            ));
            return;
        }
        if params.repo.is_none() && params.project.is_none() {
            // `--same-repo` is encoded as no target; it is only valid with a
            // source session.
            if params.source_session_id.is_none() {
                let _ = respond.send(ControlResponse::error(
                    "invalid_request",
                    "delegate requires --repo <path>, --project <id|name>, or --same-repo.",
                ));
                return;
            }
        }
        let Some(source_session_id) = params.source_session_id.clone() else {
            let _ = respond.send(ControlResponse::error(
                "invalid_source",
                "delegation requires a source agent session.",
            ));
            return;
        };
        let Some(source_session) = self
            .sessions
            .iter()
            .find(|session| session.id == source_session_id)
            .cloned()
        else {
            let _ = respond.send(ControlResponse::error(
                "invalid_source",
                format!("Source agent session \"{source_session_id}\" no longer exists."),
            ));
            return;
        };
        let group = match self.ensure_group_for_source(&source_session) {
            Ok(group) => group,
            Err(err) => {
                let _ = respond.send(ControlResponse::error(
                    "group_failed",
                    format!("Could not link the delegated worktree: {err:#}"),
                ));
                return;
            }
        };

        if let Some(existing) = self.find_duplicate_delegation(&source_session_id, &params) {
            let response = self.delegation_response(&existing, true);
            let _ = respond.send(response);
            return;
        }

        let request_key = params.request_key.clone().or_else(|| params.name.clone());
        let depth = self.delegation_depth_for_session(&source_session.id) + 1;
        if let Err(message) = self.check_delegation_policy(
            &source_session,
            &group,
            depth,
            &params,
            request_key.as_deref(),
        ) {
            let _ = respond.send(ControlResponse::error("policy_rejected", message));
            return;
        }

        let delegation_id = Uuid::new_v4().to_string();
        let brief_path = self
            .paths
            .root
            .join("delegations")
            .join(&delegation_id)
            .join("brief.md");
        if let Err(err) = self.prepare_delegation_brief(&params, &brief_path) {
            let _ = respond.send(ControlResponse::error(
                "brief_failed",
                format!("Could not prepare the delegation brief: {err:#}"),
            ));
            return;
        }
        let title = params
            .title
            .clone()
            .or_else(|| params.name.clone())
            .unwrap_or_else(|| "delegated work".to_string());
        let base_mode = params
            .base_mode
            .as_deref()
            .and_then(BaseMode::from_str)
            .unwrap_or(BaseMode::Leading);
        let delegation = Delegation {
            id: delegation_id.clone(),
            group_id: group.id.clone(),
            parent_session_id: source_session.id.clone(),
            child_session_id: None,
            depth,
            iteration: 1,
            title,
            request_key,
            brief_path: brief_path.to_string_lossy().to_string(),
            project_id: None,
            repo_path: None,
            base_mode,
            base_ref: None,
            base_label: None,
            requested_name: params.name.clone(),
            provider: params.provider.clone(),
            state: DelegationState::Queued,
            state_reason: Some("waiting for a creation slot".to_string()),
            result_id: None,
            result_status: None,
            result_summary: None,
            result_path: None,
            error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            started_at: None,
            finished_at: None,
            integrated_at: None,
            integration_commit: None,
        };
        if let Err(err) = self.session_store.upsert_delegation(&delegation) {
            let _ = respond.send(ControlResponse::error(
                "persistence_failed",
                format!("Could not save the delegation: {err}"),
            ));
            return;
        }
        self.delegations.push(delegation.clone());

        let request_id = Uuid::new_v4().to_string();
        self.delegation_request_ids
            .insert(request_id.clone(), delegation_id.clone());
        let early = if legacy_split_out {
            params.no_wait
        } else {
            params.no_wait || !params.wait_for_launch
        };
        if early {
            let _ = respond.send(self.delegation_response(&delegation, false));
        } else {
            self.pending_control_responses
                .insert(request_id.clone(), respond);
        }
        let source_label = source_session
            .title
            .clone()
            .unwrap_or_else(|| source_session.branch_name.clone());
        self.set_busy(format!(
            "Agent \"{source_label}\" asked dux to create delegated work. Queueing \"{}\"...",
            delegation.title
        ));
        let queued = QueuedDelegation {
            request_id,
            delegation_id,
            params,
            source_session: Some(source_session),
        };
        self.delegation_queue.push_back(queued);
        self.pump_delegation_queue();
    }

    fn delegation_depth_for_session(&self, session_id: &str) -> u32 {
        self.delegations
            .iter()
            .find(|delegation| delegation.child_session_id.as_deref() == Some(session_id))
            .map(|delegation| delegation.depth)
            .unwrap_or(0)
    }

    fn find_duplicate_delegation(
        &self,
        parent_session_id: &str,
        params: &SplitOutParams,
    ) -> Option<Delegation> {
        let key = params.request_key.as_deref().or(params.name.as_deref())?;
        self.delegations
            .iter()
            .find(|delegation| {
                delegation.parent_session_id == parent_session_id
                    && delegation.request_key.as_deref() == Some(key)
            })
            .cloned()
    }

    fn check_delegation_policy(
        &self,
        source: &AgentSession,
        group: &AgentGroup,
        depth: u32,
        params: &SplitOutParams,
        _request_key: Option<&str>,
    ) -> std::result::Result<(), String> {
        let config = &self.config.orchestration;
        if depth as usize > config.max_depth {
            return Err(format!(
                "Delegation depth {depth} exceeds [orchestration] max_depth = {}.",
                config.max_depth
            ));
        }
        let active_children = self
            .delegations
            .iter()
            .filter(|delegation| {
                delegation.parent_session_id == source.id && delegation.state.is_active()
            })
            .count();
        if active_children >= config.max_children_per_agent {
            return Err(format!(
                "Agent \"{}\" already has {active_children} active delegated children (max_children_per_agent = {}).",
                source.branch_name, config.max_children_per_agent
            ));
        }
        let active_descendants = self
            .delegations
            .iter()
            .filter(|delegation| delegation.group_id == group.id && delegation.state.is_active())
            .count();
        if active_descendants >= config.max_active_descendants_per_mission {
            return Err(format!(
                "Mission already has {active_descendants} active delegations (max_active_descendants_per_mission = {}).",
                config.max_active_descendants_per_mission
            ));
        }
        let total = self
            .delegations
            .iter()
            .filter(|delegation| delegation.group_id == group.id)
            .count();
        if total >= config.max_total_descendants_per_mission {
            return Err(format!(
                "Mission already has {total} delegations (max_total_descendants_per_mission = {}).",
                config.max_total_descendants_per_mission
            ));
        }
        if !config.allow_same_repo && params.repo.is_none() && params.project.is_none() {
            return Err(
                "Same-repo delegation is disabled by [orchestration] allow_same_repo.".to_string(),
            );
        }
        if !config.allow_nested && depth > 1 {
            return Err(
                "Nested delegation is disabled by [orchestration] allow_nested.".to_string(),
            );
        }
        Ok(())
    }

    fn prepare_delegation_brief(
        &mut self,
        params: &SplitOutParams,
        destination: &Path,
    ) -> Result<()> {
        let max = self.config.orchestration.brief_max_bytes;
        let mut text = if let Some(staged) = params.brief_path.as_deref() {
            let staged_path = PathBuf::from(staged);
            let pending = self.paths.root.join("delegations").join("pending");
            if !git::is_under(&pending, &staged_path) {
                bail!("staged brief path is outside {}", pending.display());
            }
            let metadata = fs::metadata(&staged_path)
                .with_context(|| format!("failed to inspect {}", staged_path.display()))?;
            if metadata.len() > max as u64 {
                bail!(
                    "delegation brief is too large ({} bytes, limit {max})",
                    metadata.len()
                );
            }
            fs::read_to_string(&staged_path)
                .with_context(|| format!("failed to read {}", staged_path.display()))?
        } else {
            params.prompt.clone().unwrap_or_default()
        };
        if text.len() > max {
            bail!(
                "delegation brief is too large ({} bytes, limit {max})",
                text.len()
            );
        }
        if text.trim().is_empty() {
            text = "(No brief was supplied.)".to_string();
        }
        let completion = "\n\n## Completion contract\n\nWhen the task is done, run:\n\n```bash\n\"$DUX_BIN\" agent finish --status success --summary-file - <<'DUX'\nOne or two paragraphs: what changed, how it was verified, and anything the parent must know before integrating.\nDUX\n```\n\nIf you need input, use `--status blocked` and put the question in the summary. If you failed, use `--status failed` and include the error.\n";
        write_delegation_file(destination, &format!("{text}{completion}"))?;
        if let Some(staged) = params.brief_path.as_deref() {
            let _ = fs::remove_file(staged);
        }
        Ok(())
    }

    fn delegation_response(&self, delegation: &Delegation, deduplicated: bool) -> ControlResponse {
        let project_name = delegation
            .project_id
            .as_deref()
            .and_then(|id| self.projects.iter().find(|project| project.id == id))
            .map(|project| project.name.clone());
        let mut response = ControlResponse::success().with_message(if deduplicated {
            format!(
                "Delegation \"{}\" already exists with state {}.",
                delegation.title,
                delegation.state.as_str()
            )
        } else {
            format!(
                "Queued delegation \"{}\" ({}).",
                delegation.title,
                delegation.state.as_str()
            )
        });
        response.delegation_id = Some(delegation.id.clone());
        response.delegation_state = Some(delegation.state.as_str().to_string());
        response.group_id = Some(delegation.group_id.clone());
        response.project = project_name;
        response.branch = delegation.base_label.clone();
        response.worktree = delegation
            .child_session_id
            .as_deref()
            .and_then(|id| self.sessions.iter().find(|session| session.id == id))
            .map(|session| session.worktree_path.clone());
        response.status = Some(delegation.state.as_str().to_string());
        if deduplicated {
            response.session_id = delegation.child_session_id.clone();
        }
        response
    }

    pub(crate) fn pump_delegation_queue(&mut self) {
        if self.create_agent_in_flight {
            return;
        }
        if let Some(plan) = self.pending_delegation_plans.pop_front() {
            self.begin_split_out_create(plan);
            return;
        }
        if self.split_out_in_flight {
            return;
        }
        let Some(item) = self.delegation_queue.pop_front() else {
            return;
        };
        self.start_delegation_prepare(item);
    }

    fn start_delegation_prepare(&mut self, item: QueuedDelegation) {
        self.split_out_in_flight = true;
        self.split_out_create_started = false;
        self.update_delegation_state(
            &item.delegation_id,
            DelegationState::Preparing,
            Some("resolving target and base".to_string()),
        );
        let projects = self.projects.clone();
        let sessions = self.sessions.clone();
        let config = self.config.clone();
        let paths = self.paths.clone();
        let worker_tx = self.worker_tx.clone();
        let request_id = item.request_id;
        let delegation_id = item.delegation_id.clone();
        let (depth, brief_path) = self
            .delegations
            .iter()
            .find(|delegation| delegation.id == item.delegation_id)
            .map(|delegation| (delegation.depth, delegation.brief_path.clone()))
            .unwrap_or((1, String::new()));
        thread::spawn(move || {
            super::workers::run_split_out_prepare_job(
                SplitOutPrepareRequest {
                    request_id,
                    delegation_id,
                    depth,
                    brief_path,
                    params: item.params,
                    source_session: item.source_session,
                    projects,
                    sessions,
                    config,
                    paths,
                },
                worker_tx,
            );
        });
    }

    pub(crate) fn update_delegation_state(
        &mut self,
        delegation_id: &str,
        state: DelegationState,
        reason: Option<String>,
    ) {
        let Some(delegation) = self
            .delegations
            .iter_mut()
            .find(|delegation| delegation.id == delegation_id)
        else {
            return;
        };
        delegation.state = state;
        delegation.state_reason = reason;
        delegation.updated_at = Utc::now();
        if state == DelegationState::Running && delegation.started_at.is_none() {
            delegation.started_at = Some(Utc::now());
        }
        let _ = self.session_store.upsert_delegation(delegation);
    }

    pub(crate) fn handle_split_out_prepared(&mut self, plan: SplitOutPlan) {
        if self
            .delegations
            .iter()
            .find(|delegation| delegation.id == plan.delegation_id)
            .is_some_and(|delegation| delegation.state == DelegationState::Cancelled)
        {
            self.abandon_split_out(&plan.request_id);
            return;
        }
        if let Some(delegation) = self
            .delegations
            .iter_mut()
            .find(|delegation| delegation.id == plan.delegation_id)
        {
            delegation.project_id = Some(plan.project.id.clone());
            delegation.repo_path = Some(plan.project.path.clone());
            delegation.base_ref = plan.base_ref.clone();
            delegation.base_label = Some(plan.base_label.clone());
            delegation.provider = plan
                .provider
                .as_ref()
                .map(|provider| provider.as_str().to_string());
            delegation.state = DelegationState::Preparing;
            delegation.state_reason = Some("creating the delegated worktree".to_string());
            delegation.updated_at = Utc::now();
            let _ = self.session_store.upsert_delegation(delegation);
        }
        if plan.project_is_new {
            self.set_busy(format!(
                "Registering \"{}\" as a project for the related worktree...",
                plan.project.name
            ));
            let project = plan.project.clone();
            let project_name = project.name.clone();
            self.pending_split_out = Some(plan);
            self.spawn_project_persistence(ProjectPersistenceAction::Add {
                project,
                status_message: format!("Registered project \"{project_name}\" for related work."),
                quiet: true,
            });
            return;
        }
        self.begin_split_out_create(plan);
    }

    pub(crate) fn finish_pending_split_out_registration(
        &mut self,
        action: &ProjectPersistenceAction,
        result: &Result<(), String>,
    ) {
        let Some(plan) = self.pending_split_out.clone() else {
            return;
        };
        let ProjectPersistenceAction::Add { project, .. } = action else {
            return;
        };
        if project.id != plan.project.id {
            return;
        }
        self.pending_split_out = None;
        match result {
            Ok(()) => self.begin_split_out_create(plan),
            Err(err) => self.fail_split_out(
                &plan.request_id,
                "registration_failed",
                format!(
                    "Could not register project \"{}\" for the related worktree: {err}",
                    plan.project.name
                ),
            ),
        }
    }

    fn begin_split_out_create(&mut self, plan: SplitOutPlan) {
        if self.create_agent_in_flight {
            // A user-created agent grabbed the creation slot while this
            // delegation was preparing. Keep the resolved plan and start it
            // as soon as that create finishes.
            self.pending_delegation_plans.push_back(plan);
            return;
        }
        let Some(source_session) = self
            .sessions
            .iter()
            .find(|session| session.id == plan.source_session_id)
            .cloned()
        else {
            self.fail_split_out(
                &plan.request_id,
                "invalid_source",
                "The source agent session no longer exists.",
            );
            return;
        };
        let source_label = source_session
            .title
            .clone()
            .unwrap_or_else(|| source_session.branch_name.clone());
        let group = match self.ensure_group_for_source(&source_session) {
            Ok(group) => group,
            Err(err) => {
                self.fail_split_out(
                    &plan.request_id,
                    "group_failed",
                    format!("Could not link the related worktree: {err:#}"),
                );
                return;
            }
        };
        let busy = format!(
            "Creating a delegated worktree for agent \"{source_label}\" in project \"{}\"...",
            plan.project.name
        );
        self.set_busy(busy.clone());
        if self
            .delegations
            .iter()
            .find(|delegation| delegation.id == plan.delegation_id)
            .is_some_and(|delegation| delegation.state == DelegationState::Cancelled)
        {
            self.abandon_split_out(&plan.request_id);
            return;
        }
        self.split_out_create_started = true;
        self.update_delegation_state(
            &plan.delegation_id,
            DelegationState::Starting,
            Some("launching the delegated agent".to_string()),
        );
        let request_id = plan.request_id.clone();
        let handoff_prompt = Some(format!(
            "You are a delegated dux agent. Read the task brief at {} and complete it. When done, run \"$DUX_BIN\" agent finish --status success --summary-file - and pass your summary on stdin. If you need a decision, use --status blocked.",
            plan.brief_path
        ));
        let root_session_id = group
            .origin_session_id
            .clone()
            .unwrap_or_else(|| source_session.id.clone());
        let request = CreateAgentRequest::SplitOut {
            request_id: plan.request_id,
            delegation_id: plan.delegation_id,
            source_session_id: source_session.id.clone(),
            source_session: Box::new(source_session.clone()),
            group_id: group.id,
            parent_session_id: source_session.id,
            root_session_id,
            depth: plan.depth,
            brief_path: plan.brief_path,
            project: plan.project,
            custom_name: plan.name,
            base_branch: Some(
                plan.base_branch
                    .clone()
                    .unwrap_or_else(|| plan.base_label.clone()),
            ),
            base_ref: plan.base_ref,
            base_label: plan.base_label,
            provider: plan.provider,
            handoff_prompt,
            project_added: plan.project_is_new,
        };
        if let Err(err) = self.dispatch_create_agent_request(request, busy) {
            self.fail_split_out(
                &request_id,
                "dispatch_failed",
                format!("Could not start related worktree creation: {err:#}"),
            );
        }
    }

    fn ensure_group_for_source(&mut self, source: &AgentSession) -> Result<AgentGroup> {
        if let Some(group_id) = source.group_id.as_deref() {
            if let Some(group) = self
                .groups
                .iter()
                .find(|group| group.id == group_id)
                .cloned()
            {
                return Ok(group);
            }
            let mut group = AgentGroup::new(
                source
                    .title
                    .clone()
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| source.branch_name.clone()),
                Some(source.id.clone()),
            );
            group.id = group_id.to_string();
            self.session_store.upsert_group(&group)?;
            self.groups.push(group.clone());
            return Ok(group);
        }

        let group = AgentGroup::new(
            source
                .title
                .clone()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| source.branch_name.clone()),
            Some(source.id.clone()),
        );
        self.session_store.upsert_group(&group)?;
        self.session_store.set_session_group(
            &source.id,
            Some(&group.id),
            source.parent_session_id.as_deref(),
        )?;
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == source.id)
        {
            session.group_id = Some(group.id.clone());
        }
        self.groups.push(group.clone());
        Ok(group)
    }

    pub(crate) fn fail_split_out(
        &mut self,
        request_id: &str,
        code: impl Into<String>,
        message: impl Into<String>,
    ) {
        let code: String = code.into();
        let message: String = message.into();
        if self.split_out_create_started {
            self.create_agent_in_flight = false;
        }
        self.split_out_create_started = false;
        self.split_out_in_flight = false;
        self.pending_split_out = None;
        if let Some(delegation_id) = self.delegation_request_ids.remove(request_id)
            && let Some(delegation) = self
                .delegations
                .iter_mut()
                .find(|delegation| delegation.id == delegation_id)
            && delegation.state != DelegationState::Cancelled
        {
            delegation.state = DelegationState::Failed;
            delegation.state_reason = Some(code.clone());
            delegation.error = Some(message.clone());
            delegation.updated_at = Utc::now();
            delegation.finished_at = Some(Utc::now());
            let _ = self.session_store.upsert_delegation(delegation);
        }
        if let Some(respond) = self.pending_control_responses.remove(request_id) {
            let _ = respond.send(ControlResponse::error(code, message.clone()));
        }
        self.set_error(message);
        let target = self.selected_left_target();
        self.rebuild_left_items();
        if let Some(target) = target {
            self.select_left_target(&target);
        }
        self.pump_delegation_queue();
    }

    fn abandon_split_out(&mut self, request_id: &str) {
        self.split_out_in_flight = false;
        self.split_out_create_started = false;
        self.pending_split_out = None;
        self.delegation_request_ids.remove(request_id);
        if let Some(respond) = self.pending_control_responses.remove(request_id) {
            let _ = respond.send(ControlResponse::error(
                "cancelled",
                "The delegation was cancelled before it launched.",
            ));
        }
        self.set_info("Delegation cancelled before launch. Its worktree was not created.");
        self.pump_delegation_queue();
    }

    pub(crate) fn complete_split_out(&mut self, request_id: &str, response: ControlResponse) {
        self.split_out_in_flight = false;
        self.split_out_create_started = false;
        self.pending_split_out = None;
        self.delegation_request_ids.remove(request_id);
        if let Some(message) = response.message.clone() {
            if response.status.as_deref() == Some("launched_with_startup_warning") {
                self.set_warning(message);
            } else {
                self.set_info(message);
            }
        }
        if let Some(respond) = self.pending_control_responses.remove(request_id) {
            let _ = respond.send(response);
        }
        let target = self.selected_left_target();
        self.rebuild_left_items();
        if let Some(target) = target {
            self.select_left_target(&target);
        }
        self.ensure_selectable_left_item();
        self.pump_delegation_queue();
    }

    pub(crate) fn control_response_for_session(
        &self,
        session: &AgentSession,
        project: &Project,
        project_added: bool,
    ) -> ControlResponse {
        let group = session
            .group_id
            .as_deref()
            .and_then(|id| self.groups.iter().find(|group| group.id == id));
        let mut response = ControlResponse::success().with_message(format!(
            "Created related agent \"{}\" in project \"{}\". It is linked under Delegated work.",
            session.branch_name, project.name
        ));
        response.session_id = Some(session.id.clone());
        response.group_id = session.group_id.clone();
        response.group_label = group.map(|group| self.related_group_label(group));
        response.project = Some(project.name.clone());
        response.project_added = Some(project_added);
        response.branch = Some(session.branch_name.clone());
        response.worktree = Some(session.worktree_path.clone());
        response.provider = Some(session.provider.as_str().to_string());
        response.status = Some("launched".to_string());
        if let Some(delegation) = self.delegation_for_session(&session.id) {
            response.delegation_id = Some(delegation.id.clone());
            response.delegation_state = Some(delegation.state.as_str().to_string());
            response.result_summary = delegation.result_summary.clone();
            response.result_path = delegation.result_path.clone();
        }
        response
    }

    pub(crate) fn reconcile_delegations_after_restore(&mut self) {
        let session_ids = self
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect::<HashSet<_>>();
        let worktrees = self
            .sessions
            .iter()
            .map(|session| {
                (
                    session.id.clone(),
                    Path::new(&session.worktree_path).exists(),
                )
            })
            .collect::<HashMap<_, _>>();
        for delegation in &mut self.delegations {
            if !delegation.state.is_active() {
                continue;
            }
            let next = match delegation.child_session_id.as_deref() {
                Some(child) if !session_ids.contains(child) => DelegationState::Failed,
                Some(child) if !worktrees.get(child).copied().unwrap_or(false) => {
                    DelegationState::Failed
                }
                Some(_) => DelegationState::Interrupted,
                None => DelegationState::Interrupted,
            };
            if next != delegation.state {
                delegation.state = next;
                delegation.state_reason = Some(if next == DelegationState::Failed {
                    "child session or worktree disappeared while dux was not running".to_string()
                } else {
                    "dux restarted while the delegation was in flight; reattach the child to continue"
                        .to_string()
                });
                delegation.updated_at = Utc::now();
            }
            if delegation.result_id.is_none() && delegation.state != DelegationState::Cancelled {
                let path = self
                    .paths
                    .root
                    .join("delegations")
                    .join(&delegation.id)
                    .join("result.json");
                if let Ok(raw) = fs::read_to_string(&path)
                    && let Ok(result) = serde_json::from_str::<DelegationResult>(&raw)
                {
                    delegation.result_id = Some(if result.result_id.is_empty() {
                        Uuid::new_v4().to_string()
                    } else {
                        result.result_id.clone()
                    });
                    delegation.result_status = Some(result.status.clone());
                    delegation.result_summary = Some(truncate_summary(&result.summary, 8192));
                    delegation.result_path = Some(path.to_string_lossy().to_string());
                    delegation.state = result.state();
                    delegation.state_reason = None;
                    delegation.error = None;
                    delegation.finished_at = Some(Utc::now());
                    delegation.updated_at = Utc::now();
                }
            }
            let _ = self.session_store.upsert_delegation(delegation);
        }
    }

    /// Import a result file whose callback was lost while dux was busy. The
    /// CLI writes the file before sending the control request, so polling
    /// status is enough to recover a missed finish without a background timer.
    fn import_pending_delegation_results(&mut self) {
        let mut imported = false;
        for delegation in &mut self.delegations {
            if delegation.state == DelegationState::Cancelled
                || delegation.state == DelegationState::Resuming
                || delegation.result_id.is_some()
            {
                continue;
            }
            let path = self
                .paths
                .root
                .join("delegations")
                .join(&delegation.id)
                .join("result.json");
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if metadata.len() > self.config.orchestration.result_max_bytes as u64 {
                continue;
            }
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(result) = serde_json::from_str::<DelegationResult>(&raw) else {
                // The CLI may still be writing the file. The next poll retries.
                continue;
            };
            delegation.result_id = Some(if result.result_id.is_empty() {
                Uuid::new_v4().to_string()
            } else {
                result.result_id.clone()
            });
            delegation.result_status = Some(result.status.clone());
            delegation.result_summary = Some(truncate_summary(
                &result.summary,
                self.config.orchestration.result_max_bytes.min(8192),
            ));
            delegation.result_path = Some(path.to_string_lossy().to_string());
            delegation.state = result.state();
            delegation.state_reason = None;
            delegation.error = None;
            delegation.finished_at = Some(Utc::now());
            delegation.updated_at = Utc::now();
            let _ = self.session_store.upsert_delegation(delegation);
            imported = true;
        }
        if imported {
            self.rebuild_left_items();
        }
    }

    fn fail_delegation_result_write(&mut self, delegation_id: &str, message: &str) {
        let mut changed = false;
        if let Some(delegation) = self
            .delegations
            .iter_mut()
            .find(|delegation| delegation.id == delegation_id)
            && delegation.state != DelegationState::Cancelled
        {
            delegation.state = DelegationState::Failed;
            delegation.state_reason = Some("result_write_failed".to_string());
            delegation.error = Some(message.to_string());
            delegation.finished_at = Some(Utc::now());
            delegation.updated_at = Utc::now();
            let _ = self.session_store.upsert_delegation(delegation);
            changed = true;
        }
        if changed {
            self.rebuild_left_items();
            self.set_error(format!(
                "A delegated agent finished, but its result could not be stored: {message}"
            ));
        }
    }

    pub(crate) fn delegation_for_session(&self, session_id: &str) -> Option<&Delegation> {
        self.delegations
            .iter()
            .find(|delegation| delegation.is_for_session(session_id))
    }

    pub(crate) fn related_group_label(&self, group: &AgentGroup) -> String {
        related_group_label_for(&self.sessions, group)
    }

    pub(crate) fn cancel_selected_delegation(&mut self) -> Result<()> {
        let Some(session) = self.selected_session().cloned() else {
            self.set_warning("Select a delegated agent first.");
            return Ok(());
        };
        let response = self.handle_delegation_cancel(None, Some(&session.id), None, false);
        if response.ok {
            if let Some(message) = response.message {
                self.set_info(message);
            }
        } else if let Some(message) = response.error {
            self.set_error(message);
        }
        Ok(())
    }

    pub(crate) fn integrate_selected_delegation(&mut self) -> Result<()> {
        let Some(session) = self.selected_session().cloned() else {
            self.set_warning("Select a completed delegated agent first.");
            return Ok(());
        };
        let (tx, rx) = mpsc::channel();
        self.handle_delegation_integrate(None, session.id, tx);
        if let Ok(response) = rx.try_recv() {
            if response.ok {
                if let Some(message) = response.message {
                    self.set_info(message);
                }
            } else if let Some(message) = response.error {
                self.set_error(message);
            }
        }
        Ok(())
    }

    pub(crate) fn select_next_related_agent(&mut self) -> Result<()> {
        let Some(current) = self.selected_session().cloned() else {
            self.set_warning("Select an agent first to jump to a related agent.");
            return Ok(());
        };
        let Some(group_id) = current.group_id.clone() else {
            self.set_warning(format!(
                "Agent \"{}\" is not part of a related-work group.",
                current.branch_name
            ));
            return Ok(());
        };
        let members = self
            .sessions
            .iter()
            .filter(|session| session.group_id.as_deref() == Some(group_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if members.len() < 2 {
            self.set_warning(format!(
                "Agent \"{}\" has no other related agents yet.",
                current.branch_name
            ));
            return Ok(());
        }
        let current_index = members
            .iter()
            .position(|session| session.id == current.id)
            .unwrap_or(0);
        let next = &members[(current_index + 1) % members.len()];
        let target = LeftSelectionTarget::Session(next.id.clone());
        let previous = self.selected_left_target();
        let project_name = self
            .projects
            .iter()
            .find(|project| project.id == next.project_id)
            .map(|project| project.name.clone())
            .unwrap_or_else(|| next.project_id.clone());
        if !self.select_left_target(&target) {
            self.reset_left_search_state();
            self.rebuild_left_items();
            let _ = self.select_left_target(&target);
        }
        self.refresh_after_left_selection_change(&previous);
        self.set_info(format!(
            "Switched to related agent \"{}\" in project \"{}\".",
            next.title
                .clone()
                .unwrap_or_else(|| next.branch_name.clone()),
            project_name
        ));
        Ok(())
    }

    pub(crate) fn session_is_related_member(&self, session: &AgentSession) -> bool {
        session.parent_session_id.is_some() && self.group_for_session(session).is_some()
    }

    /// Whether the session is the origin agent of a group that has split-out
    /// children and therefore renders under Delegated work.
    pub(crate) fn session_is_related_origin(&self, session: &AgentSession) -> bool {
        session.parent_session_id.is_none()
            && self.group_for_session(session).is_some_and(|group| {
                self.sessions.iter().any(|candidate| {
                    candidate.group_id.as_deref() == Some(group.id.as_str())
                        && candidate.parent_session_id.is_some()
                })
            })
    }

    pub(crate) fn group_for_session(&self, session: &AgentSession) -> Option<&AgentGroup> {
        session
            .group_id
            .as_deref()
            .and_then(|id| self.groups.iter().find(|group| group.id == id))
    }

    fn find_delegation(
        &self,
        source_session_id: Option<&str>,
        session_id: Option<&str>,
        delegation_id: Option<&str>,
    ) -> Option<&Delegation> {
        if let Some(id) = delegation_id {
            return self
                .delegations
                .iter()
                .find(|delegation| delegation.id == id);
        }
        let session = session_id.or(source_session_id)?;
        self.delegation_for_session(session)
    }

    fn delegation_is_descendant_of(&self, delegation: &Delegation, ancestor: &str) -> bool {
        let mut current = Some(delegation);
        let mut guard = 0;
        while let Some(delegation) = current {
            if delegation.parent_session_id == ancestor {
                return true;
            }
            guard += 1;
            if guard > 64 {
                return false;
            }
            current = self.delegations.iter().find(|candidate| {
                candidate.child_session_id.as_deref() == Some(&delegation.parent_session_id)
            });
        }
        false
    }

    pub(crate) fn delegation_subtree(&self, root_session_id: &str) -> Vec<Delegation> {
        let mut included = HashSet::new();
        let mut frontier = vec![root_session_id.to_string()];
        if let Some(own) = self.delegation_for_session(root_session_id) {
            included.insert(own.id.clone());
        }
        let mut guard = 0;
        while !frontier.is_empty() && guard < 256 {
            guard += 1;
            let parents = std::mem::take(&mut frontier);
            for delegation in &self.delegations {
                if parents.contains(&delegation.parent_session_id) {
                    included.insert(delegation.id.clone());
                    if let Some(child) = delegation.child_session_id.as_ref() {
                        frontier.push(child.clone());
                    }
                }
            }
        }
        let mut result = self
            .delegations
            .iter()
            .filter(|delegation| included.contains(&delegation.id))
            .cloned()
            .collect::<Vec<_>>();
        result.sort_by_key(|delegation| (delegation.depth, delegation.created_at));
        result
    }

    fn delegation_info(&self, delegation: &Delegation) -> crate::ipc::DelegationInfo {
        let child = delegation
            .child_session_id
            .as_deref()
            .and_then(|id| self.sessions.iter().find(|session| session.id == id));
        let project_name = delegation
            .project_id
            .as_deref()
            .and_then(|id| self.projects.iter().find(|project| project.id == id))
            .map(|project| project.name.clone());
        crate::ipc::DelegationInfo {
            id: delegation.id.clone(),
            parent_session_id: delegation.parent_session_id.clone(),
            child_session_id: delegation.child_session_id.clone(),
            title: delegation.title.clone(),
            state: delegation.state.as_str().to_string(),
            state_reason: delegation.state_reason.clone(),
            project_id: delegation.project_id.clone(),
            project_name,
            repo_path: delegation.repo_path.clone(),
            branch: child.map(|session| session.branch_name.clone()),
            worktree: child.map(|session| session.worktree_path.clone()),
            base_mode: delegation.base_mode.as_str().to_string(),
            base_ref: delegation.base_ref.clone(),
            base_label: delegation.base_label.clone(),
            depth: delegation.depth,
            iteration: delegation.iteration,
            provider: delegation.provider.clone(),
            result_status: delegation.result_status.clone(),
            result_summary: delegation
                .result_summary
                .clone()
                .map(|summary| truncate_summary(&summary, 512)),
            result_path: delegation.result_path.clone(),
            error: delegation.error.clone(),
            created_at: delegation.created_at.to_rfc3339(),
            updated_at: delegation.updated_at.to_rfc3339(),
            started_at: delegation.started_at.map(|time| time.to_rfc3339()),
            finished_at: delegation.finished_at.map(|time| time.to_rfc3339()),
            integrated_at: delegation.integrated_at.map(|time| time.to_rfc3339()),
            integration_commit: delegation.integration_commit.clone(),
        }
    }

    pub(crate) fn delegation_status_response(
        &self,
        source_session_id: Option<&str>,
        session_id: Option<&str>,
        delegation_id: Option<&str>,
    ) -> ControlResponse {
        let target = session_id.or(source_session_id);
        let delegations = if let Some(id) = delegation_id {
            let Some(delegation) = self
                .delegations
                .iter()
                .find(|delegation| delegation.id == id)
                .cloned()
            else {
                return ControlResponse::error(
                    "delegation_not_found",
                    "No delegation matches that id.",
                );
            };
            vec![delegation]
        } else if let Some(session) = target {
            self.delegation_subtree(session)
        } else {
            return ControlResponse::error(
                "invalid_target",
                "Pass --session, --delegation, or run inside a dux agent.",
            );
        };
        let mut response = ControlResponse::success();
        response.delegations = delegations
            .iter()
            .map(|delegation| self.delegation_info(delegation))
            .collect();
        if response.delegations.is_empty() {
            response.message = Some("This agent has no delegations.".to_string());
        } else {
            response.message = Some(format!("{} delegation(s).", response.delegations.len()));
        }
        response
    }

    fn delegation_result_response(
        &self,
        source_session_id: Option<&str>,
        session_id: Option<&str>,
        delegation_id: Option<&str>,
    ) -> ControlResponse {
        let Some(delegation) = self.find_delegation(source_session_id, session_id, delegation_id)
        else {
            return ControlResponse::error(
                "delegation_not_found",
                "No delegation matches that session or delegation id.",
            );
        };
        let result_path = if delegation.state.is_active() && delegation.result_summary.is_none() {
            None
        } else {
            delegation.result_path.clone().or_else(|| {
                let dir = self.paths.root.join("delegations").join(&delegation.id);
                let current = dir.join("result.json");
                if current.exists() {
                    return Some(current.to_string_lossy().to_string());
                }
                (1..=delegation.iteration).rev().find_map(|iteration| {
                    let path = dir.join(format!("result-{iteration}.json"));
                    path.exists().then(|| path.to_string_lossy().to_string())
                })
            })
        };
        let mut response = ControlResponse::success().with_message(format!(
            "Delegation \"{}\" is {}.",
            delegation.title,
            delegation.state.as_str()
        ));
        response.delegation_id = Some(delegation.id.clone());
        response.delegation_state = Some(delegation.state.as_str().to_string());
        response.result_summary = delegation.result_summary.clone();
        response.result_path = result_path;
        response
    }

    pub(crate) fn handle_delegation_finish(
        &mut self,
        source_session_id: Option<&str>,
        session_id: Option<&str>,
        delegation_id: Option<&str>,
        mut result: DelegationResult,
        result_path: Option<String>,
    ) -> ControlResponse {
        let target_session = session_id.or(source_session_id);
        let Some(delegation_index) = self.delegations.iter().position(|delegation| {
            delegation_id.is_some_and(|id| delegation.id == id)
                || target_session
                    .is_some_and(|id| delegation.child_session_id.as_deref() == Some(id))
        }) else {
            return ControlResponse::error(
                "delegation_not_found",
                "No delegation matches that session or delegation id.",
            );
        };
        if let (Some(source), Some(child)) = (
            source_session_id,
            self.delegations[delegation_index]
                .child_session_id
                .as_deref(),
        ) && source != child
        {
            return ControlResponse::error(
                "not_owner",
                "Only the delegated child (or a human invocation) may report this result.",
            );
        }
        if self.delegations[delegation_index]
            .child_session_id
            .is_none()
            && let Some(source) = source_session_id.or(session_id)
        {
            // The child can report before the TUI processes its launch event.
            // DUX_DELEGATION_ID is authoritative, so bind the session now.
            self.delegations[delegation_index].child_session_id = Some(source.to_string());
        }
        let delegation_id = self.delegations[delegation_index].id.clone();
        let expected_dir = self.paths.root.join("delegations").join(&delegation_id);
        let expected_path = expected_dir.join("result.json");
        let path = result_path
            .clone()
            .unwrap_or_else(|| expected_path.to_string_lossy().to_string());
        if path.len() > 4096
            || (Path::new(&path) != expected_path
                && !git::is_under(&expected_dir, Path::new(&path)))
        {
            return ControlResponse::error(
                "invalid_result",
                "Result path is outside the delegation's result directory.",
            );
        }
        if let Some(path) = result_path.as_deref()
            && let Ok(raw) = fs::read_to_string(path)
            && let Ok(stored) = serde_json::from_str::<DelegationResult>(&raw)
        {
            // The CLI may send a compact copy to stay under the control-message
            // limit. The durable file is authoritative.
            result = stored;
        }
        let result_id = if result.result_id.trim().is_empty() {
            Uuid::new_v4().to_string()
        } else {
            result.result_id.clone()
        };
        let encoded = match serde_json::to_string_pretty(&result) {
            Ok(encoded) => encoded,
            Err(err) => {
                return ControlResponse::error(
                    "invalid_result",
                    format!("Could not encode the delegation result: {err}"),
                );
            }
        };
        if encoded.len() > self.config.orchestration.result_max_bytes {
            let message = format!(
                "Result is too large ({} bytes, limit {}).",
                encoded.len(),
                self.config.orchestration.result_max_bytes
            );
            self.fail_delegation_result_write(&delegation_id, &message);
            return ControlResponse::error("result_too_large", message);
        }
        if let Err(err) = write_delegation_file(Path::new(&path), &encoded) {
            let message = err.to_string();
            self.fail_delegation_result_write(&delegation_id, &message);
            return ControlResponse::error("result_write_failed", message);
        }
        let delegation = &mut self.delegations[delegation_index];
        let was_cancelled = delegation.state == DelegationState::Cancelled;
        delegation.result_id = Some(result_id);
        delegation.result_status = Some(result.status.clone());
        delegation.result_summary = Some(truncate_summary(
            &result.summary,
            self.config.orchestration.result_max_bytes.min(8192),
        ));
        delegation.result_path = Some(path.clone());
        delegation.error = None;
        delegation.updated_at = Utc::now();
        if !was_cancelled && delegation.state != DelegationState::Resuming {
            delegation.state = result.state();
            delegation.state_reason = None;
            delegation.finished_at = Some(Utc::now());
        }
        let state = delegation.state;
        let summary = delegation.result_summary.clone();
        let title = delegation.title.clone();
        let _ = self.session_store.upsert_delegation(delegation);
        self.rebuild_left_items();
        let status_message = format!(
            "Delegated agent \"{title}\" reported {}. Open its result for details.",
            result.status
        );
        match state {
            DelegationState::Blocked => self.set_warning(status_message),
            DelegationState::Failed => self.set_error(status_message),
            _ => self.set_info(status_message),
        }
        let mut response = ControlResponse::success().with_message(if was_cancelled {
            format!(
                "Stored a late result for cancelled delegation \"{title}\"; its state was left cancelled."
            )
        } else {
            format!(
                "Delegation \"{title}\" reported {}.",
                result.status
            )
        });
        response.delegation_id = Some(delegation_id);
        response.delegation_state = Some(state.as_str().to_string());
        response.result_summary = summary;
        response.result_path = Some(path);
        response
    }

    fn handle_delegation_followup(
        &mut self,
        source_session_id: Option<&str>,
        session_id: Option<&str>,
        delegation_id: Option<&str>,
        text: &str,
        submit: bool,
        message_only: bool,
    ) -> ControlResponse {
        self.import_pending_delegation_results();
        if text.trim().is_empty() {
            return ControlResponse::error("invalid_text", "Follow-up text is empty.");
        }
        let Some(delegation) = self
            .find_delegation(source_session_id, session_id, delegation_id)
            .cloned()
        else {
            return ControlResponse::error(
                "delegation_not_found",
                "No delegation matches that session or delegation id.",
            );
        };
        if let Some(source) = source_session_id
            && !self.delegation_is_descendant_of(&delegation, source)
        {
            return ControlResponse::error(
                "not_ancestor",
                "A parent may only send follow-ups to its own delegated descendants.",
            );
        }
        let Some(child_session_id) = delegation.child_session_id.as_deref() else {
            return ControlResponse::error(
                "not_running",
                "The delegated agent has not launched yet.",
            );
        };
        let Some(provider) = self
            .providers
            .get(child_session_id)
            .filter(|provider| !provider.is_exited())
        else {
            return ControlResponse::error(
                "not_running",
                "The delegated agent's PTY is not running. Reconnect it before sending a follow-up.",
            );
        };
        let inbox = self
            .paths
            .root
            .join("delegations")
            .join(&delegation.id)
            .join("inbox.md");
        if let Some(parent) = inbox.parent() {
            let _ = fs::create_dir_all(parent);
        }
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&inbox)
        {
            let _ = writeln!(file, "\n## Follow-up\n\n{text}\n");
        }
        let flattened = text.replace(['\r', '\n'], " ");
        let mut payload = flattened.replace('\0', "").into_bytes();
        if submit && !payload.ends_with(b"\r") {
            payload.push(b'\r');
        }
        if let Err(err) = provider.write_bytes(&payload) {
            return ControlResponse::error(
                "write_failed",
                format!("Could not write to the delegated agent: {err}"),
            );
        }
        if message_only {
            let mut response = ControlResponse::success().with_message(format!(
                "Sent a message to delegated agent \"{}\".",
                delegation.title
            ));
            response.delegation_id = Some(delegation.id);
            response.delegation_state = Some(delegation.state.as_str().to_string());
            return response;
        }
        let state = if delegation.state.is_resumable() {
            DelegationState::Running
        } else {
            delegation.state
        };
        if let Some(row) = self
            .delegations
            .iter_mut()
            .find(|candidate| candidate.id == delegation.id)
        {
            if row.state.is_resumable() {
                let result_path = self
                    .paths
                    .root
                    .join("delegations")
                    .join(&row.id)
                    .join("result.json");
                if result_path.exists() {
                    let archive =
                        result_path.with_file_name(format!("result-{}.json", row.iteration));
                    let _ = fs::rename(&result_path, archive);
                }
                row.iteration = row.iteration.saturating_add(1);
                row.result_id = None;
                row.result_status = None;
                row.result_summary = None;
                row.result_path = None;
                row.finished_at = None;
                row.integrated_at = None;
                row.integration_commit = None;
            }
            row.state = state;
            row.state_reason = Some("follow-up delivered".to_string());
            row.updated_at = Utc::now();
            let _ = self.session_store.upsert_delegation(row);
        }
        let mut response = ControlResponse::success().with_message(format!(
            "Sent a follow-up to delegated agent \"{}\".",
            delegation.title
        ));
        response.delegation_id = Some(delegation.id);
        response.delegation_state = Some(state.as_str().to_string());
        response
    }

    pub(crate) fn handle_delegation_cancel(
        &mut self,
        source_session_id: Option<&str>,
        session_id: Option<&str>,
        delegation_id: Option<&str>,
        subtree: bool,
    ) -> ControlResponse {
        let targets = if let Some(id) = delegation_id {
            if let Some(delegation) = self
                .delegations
                .iter()
                .find(|delegation| delegation.id == id)
                .cloned()
            {
                if subtree {
                    delegation
                        .child_session_id
                        .as_deref()
                        .map(|child| self.delegation_subtree(child))
                        .unwrap_or_else(|| vec![delegation])
                } else {
                    vec![delegation]
                }
            } else {
                Vec::new()
            }
        } else if let Some(target) = session_id.or(source_session_id) {
            if subtree {
                self.delegation_subtree(target)
            } else {
                self.delegation_for_session(target)
                    .cloned()
                    .into_iter()
                    .collect()
            }
        } else {
            Vec::new()
        };
        if targets.is_empty() {
            return ControlResponse::error(
                "delegation_not_found",
                "No delegation matches that session or delegation id.",
            );
        }
        if let Some(source) = source_session_id
            && targets
                .iter()
                .any(|delegation| !self.delegation_is_descendant_of(delegation, source))
        {
            return ControlResponse::error(
                "not_ancestor",
                "A parent may only cancel its own delegated descendants.",
            );
        }
        let mut cancelled = Vec::new();
        for target in targets {
            let Some(row) = self.delegations.iter_mut().find(|row| row.id == target.id) else {
                continue;
            };
            if !row.state.is_active() {
                continue;
            }
            row.state = DelegationState::Cancelled;
            row.state_reason = Some("cancelled by parent".to_string());
            row.finished_at = Some(Utc::now());
            row.updated_at = Utc::now();
            let _ = self.session_store.upsert_delegation(row);
            if let Some(child) = row.child_session_id.clone() {
                self.providers.remove(&child);
                self.running_provider_pins.remove(&child);
                self.last_pty_activity.remove(&child);
                self.cancel_provider_session_id_discovery(&child);
                self.mark_session_desired_running(&child, false);
                self.mark_session_status(&child, SessionStatus::Detached);
            }
            let request_ids = self
                .delegation_request_ids
                .iter()
                .filter(|(_, delegation_id)| *delegation_id == &target.id)
                .map(|(request_id, _)| request_id.clone())
                .collect::<Vec<_>>();
            for request_id in request_ids {
                self.delegation_request_ids.remove(&request_id);
                if let Some(respond) = self.pending_control_responses.remove(&request_id) {
                    let _ = respond.send(ControlResponse::error(
                        "cancelled",
                        "The delegation was cancelled before it launched.",
                    ));
                }
            }
            cancelled.push(target.id);
        }
        self.delegation_queue
            .retain(|item| !cancelled.contains(&item.delegation_id));
        let pending_before = self.pending_delegation_plans.len();
        self.pending_delegation_plans
            .retain(|plan| !cancelled.contains(&plan.delegation_id));
        if self.pending_delegation_plans.len() < pending_before {
            // The prepared plan was waiting for an unrelated create. Release
            // the delegation pipeline slot so queued work can proceed.
            self.split_out_in_flight = false;
            self.pump_delegation_queue();
        }
        self.rebuild_left_items();
        let message = format!(
            "Cancelled {} delegated agent(s). Their worktrees were kept.",
            cancelled.len()
        );
        self.set_info(message.clone());
        ControlResponse::success().with_message(message)
    }

    fn handle_delegation_retry(&mut self, delegation_id: &str) -> ControlResponse {
        let Some(old) = self
            .delegations
            .iter()
            .find(|delegation| delegation.id == delegation_id)
            .cloned()
        else {
            return ControlResponse::error(
                "delegation_not_found",
                "No delegation matches that id.",
            );
        };
        if old.child_session_id.is_some()
            || !matches!(
                old.state,
                DelegationState::Failed | DelegationState::Interrupted
            )
        {
            return ControlResponse::error(
                "not_retryable",
                format!(
                    "Delegation is {} and already has a child session; cancel or delete it before retrying.",
                    old.state.as_str()
                ),
            );
        }
        if let Some(repo) = old.repo_path.as_deref()
            && let Some(name) = old.requested_name.as_deref()
            && git::branch_exists(Path::new(repo), name).is_some()
        {
            return ControlResponse::error(
                "branch_exists",
                format!("Branch \"{name}\" still exists; delete its worktree before retrying."),
            );
        }
        let prompt = fs::read_to_string(&old.brief_path).ok();
        let params = SplitOutParams {
            source_session_id: Some(old.parent_session_id.clone()),
            repo: old.repo_path.clone(),
            project: old.project_id.clone(),
            name: old.requested_name.clone(),
            base: (old.base_mode == BaseMode::Ref)
                .then(|| old.base_ref.clone())
                .flatten(),
            provider: old.provider.clone(),
            prompt,
            timeout_ms: None,
            title: Some(old.title.clone()),
            base_mode: Some(old.base_mode.as_str().to_string()),
            base_session: None,
            request_key: Some(Uuid::new_v4().to_string()),
            no_wait: true,
            wait_for_launch: false,
            brief_path: None,
            brief_sha256: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.handle_split_out_request(params, tx, false);
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(response) => response,
            Err(_) => ControlResponse::error(
                "retry_failed",
                "The retry was queued but did not return a delegation id in time.",
            ),
        }
    }

    fn handle_delegation_integrate(
        &mut self,
        source_session_id: Option<&str>,
        session_id: String,
        respond: Sender<ControlResponse>,
    ) {
        self.import_pending_delegation_results();
        if source_session_id.is_some() && !self.config.orchestration.allow_agent_integration {
            let _ = respond.send(ControlResponse::error(
                "integration_requires_user",
                "Agent-initiated integration is disabled. A human can integrate from the CLI or UI.",
            ));
            return;
        }
        let Some(delegation) = self.delegation_for_session(&session_id).cloned() else {
            let _ = respond.send(ControlResponse::error(
                "delegation_not_found",
                "That session is not a delegated child.",
            ));
            return;
        };
        if delegation.state != DelegationState::Completed {
            let _ = respond.send(ControlResponse::error(
                "not_completed",
                format!(
                    "Delegation is {}; only completed children can be integrated.",
                    delegation.state.as_str()
                ),
            ));
            return;
        }
        if delegation.integrated_at.is_some() {
            let _ = respond.send(ControlResponse::success().with_message(format!(
                "Delegation \"{}\" was already integrated{}.",
                delegation.title,
                delegation
                    .integration_commit
                    .as_deref()
                    .map(|commit| {
                        format!(" ({})", commit.chars().take(7).collect::<String>())
                    })
                    .unwrap_or_default()
            )));
            return;
        }
        let Some(child) = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
        else {
            let _ = respond.send(ControlResponse::error(
                "invalid_session",
                "The delegated session no longer exists.",
            ));
            return;
        };
        let Some(parent) = self
            .sessions
            .iter()
            .find(|session| session.id == delegation.parent_session_id)
            .cloned()
        else {
            let _ = respond.send(ControlResponse::error(
                "invalid_parent",
                "The parent session no longer exists.",
            ));
            return;
        };
        if parent.project_id != child.project_id {
            let _ = respond.send(ControlResponse::error(
                "cross_repo",
                "Cross-repository delegations land through commits and pull requests; dux does not merge their histories.",
            ));
            return;
        }
        if self.providers.contains_key(&parent.id) {
            let _ = respond.send(ControlResponse::error(
                "parent_running",
                "Stop the parent agent before integrating so its files do not change underneath it.",
            ));
            return;
        }
        let Some(project) = self
            .projects
            .iter()
            .find(|project| project.id == parent.project_id)
            .cloned()
        else {
            let _ = respond.send(ControlResponse::error(
                "invalid_project",
                "The parent project is no longer registered.",
            ));
            return;
        };
        let request_id = Uuid::new_v4().to_string();
        self.pending_control_responses
            .insert(request_id.clone(), respond);
        self.set_busy(format!(
            "Integrating delegated agent \"{}\" into \"{}\"...",
            child.branch_name, parent.branch_name
        ));
        let worker_tx = self.worker_tx.clone();
        thread::spawn(move || {
            super::workers::run_delegation_integrate_job(
                request_id, delegation, child, parent, project, worker_tx,
            );
        });
    }

    pub(crate) fn finish_delegation_integration(
        &mut self,
        request_id: String,
        delegation_id: String,
        result: std::result::Result<DelegationIntegrationOutcome, String>,
    ) {
        match result {
            Ok(outcome) => {
                if let Some(delegation) = self
                    .delegations
                    .iter_mut()
                    .find(|delegation| delegation.id == delegation_id)
                {
                    delegation.integrated_at = Some(Utc::now());
                    delegation.integration_commit = outcome.commit.clone();
                    delegation.updated_at = Utc::now();
                    let _ = self.session_store.upsert_delegation(delegation);
                }
                logger::info(&format!(
                    "delegation integration completed via {}: {}",
                    outcome.strategy, outcome.message
                ));
                let response = ControlResponse::success().with_message(outcome.message);
                if let Some(respond) = self.pending_control_responses.remove(&request_id) {
                    let _ = respond.send(response.clone());
                }
                self.set_info(response.message.unwrap_or_default());
                self.reload_changed_files_quiet();
            }
            Err(message) => {
                if let Some(respond) = self.pending_control_responses.remove(&request_id) {
                    let _ =
                        respond.send(ControlResponse::error("integrate_failed", message.clone()));
                }
                self.set_error(message);
            }
        }
    }

    pub(crate) fn related_agents_response(
        &self,
        source_session_id: Option<&str>,
    ) -> ControlResponse {
        let Some(source_session_id) = source_session_id else {
            return ControlResponse::error(
                "invalid_source",
                "No source session was supplied. Set DUX_SESSION_ID or pass --session.",
            );
        };
        let Some(source) = self
            .sessions
            .iter()
            .find(|session| session.id == source_session_id)
        else {
            return ControlResponse::error(
                "invalid_source",
                format!("Agent session \"{source_session_id}\" no longer exists."),
            );
        };
        let mut response = ControlResponse::success();
        response.session_id = Some(source.id.clone());
        let Some(group_id) = source.group_id.as_deref() else {
            response.message = Some(format!(
                "Agent \"{}\" is not part of a related-work group.",
                source.branch_name
            ));
            return response;
        };
        let group = self.groups.iter().find(|group| group.id == group_id);
        response.group_id = Some(group_id.to_string());
        response.group_label = group.map(|group| self.related_group_label(group));
        response.related = self
            .sessions
            .iter()
            .filter(|session| session.group_id.as_deref() == Some(group_id))
            .map(|session| RelatedAgentInfo {
                session_id: session.id.clone(),
                project_id: session.project_id.clone(),
                project_name: self
                    .projects
                    .iter()
                    .find(|project| project.id == session.project_id)
                    .map(|project| project.name.clone())
                    .unwrap_or_else(|| session.project_id.clone()),
                label: session
                    .title
                    .clone()
                    .unwrap_or_else(|| session.branch_name.clone()),
                branch_name: session.branch_name.clone(),
                worktree_path: session.worktree_path.clone(),
                provider: session.provider.as_str().to_string(),
                status: session.status.as_str().to_string(),
                is_origin: session.parent_session_id.is_none(),
            })
            .collect();
        response.message = Some(format!(
            "Agent \"{}\" has {} related agent(s).",
            source.branch_name,
            response.related.len()
        ));
        response
    }
}

fn write_delegation_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    Ok(())
}

fn truncate_summary(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        if out.len() + ch.len_utf8() > max_bytes {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

/// Environment variables injected into agent PTYs so the `dux` CLI can talk
/// back to the running TUI.
pub(crate) fn agent_control_env(paths: &DuxPaths, session: &AgentSession) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "DUX_HOME".to_string(),
            paths.root.to_string_lossy().to_string(),
        ),
        (
            "DUX_SOCKET".to_string(),
            paths.control_socket_path.to_string_lossy().to_string(),
        ),
        ("DUX_SESSION_ID".to_string(), session.id.clone()),
        ("DUX_AGENT_ID".to_string(), session.id.clone()),
        ("DUX_PROJECT_ID".to_string(), session.project_id.clone()),
        (
            "DUX_WORKTREE_PATH".to_string(),
            session.worktree_path.clone(),
        ),
        ("DUX_BRANCH".to_string(), session.branch_name.clone()),
        (
            "DUX_PROVIDER".to_string(),
            session.provider.as_str().to_string(),
        ),
        (
            "DUX_AGENT_TITLE".to_string(),
            session
                .title
                .clone()
                .unwrap_or_else(|| session.branch_name.clone()),
        ),
    ];
    if let Some(project_path) = session.project_path.as_deref() {
        env.push(("DUX_PROJECT_PATH".to_string(), project_path.to_string()));
    }
    if let Ok(exe) = std::env::current_exe() {
        env.push(("DUX_BIN".to_string(), exe.to_string_lossy().to_string()));
    }
    env
}

/// Extra environment for a delegated child. The brief and result paths are
/// stable for the life of the delegation, including follow-up iterations.
pub(crate) fn delegation_control_env(
    paths: &DuxPaths,
    request: &SplitOutCreateRequest,
) -> Vec<(String, String)> {
    let dir = paths.root.join("delegations").join(&request.delegation_id);
    vec![
        (
            "DUX_DELEGATION_ID".to_string(),
            request.delegation_id.clone(),
        ),
        (
            "DUX_PARENT_SESSION_ID".to_string(),
            request.parent_session_id.clone(),
        ),
        (
            "DUX_ROOT_SESSION_ID".to_string(),
            request.root_session_id.clone(),
        ),
        ("DUX_GROUP_ID".to_string(), request.group_id.clone()),
        (
            "DUX_DELEGATION_DEPTH".to_string(),
            request.depth.to_string(),
        ),
        ("DUX_BRIEF_FILE".to_string(), request.brief_path.clone()),
        (
            "DUX_RESULT_FILE".to_string(),
            dir.join("result.json").to_string_lossy().to_string(),
        ),
        (
            "DUX_INBOX_FILE".to_string(),
            dir.join("inbox.md").to_string_lossy().to_string(),
        ),
        (
            "DUX_PARENT_WORKTREE".to_string(),
            request.source_session.worktree_path.clone(),
        ),
        (
            "DUX_PARENT_BRANCH".to_string(),
            request.source_session.branch_name.clone(),
        ),
    ]
}
