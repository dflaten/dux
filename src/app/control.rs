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
            ControlCommand::SplitOut(params) => {
                self.handle_split_out_request(params, respond);
            }
        }
    }

    fn handle_split_out_request(
        &mut self,
        params: SplitOutParams,
        respond: Sender<ControlResponse>,
    ) {
        if self.split_out_in_flight {
            let _ = respond.send(ControlResponse::error(
                "busy",
                "Another related worktree is already being created. Wait for it to finish, then retry.",
            ));
            return;
        }
        if self.create_agent_in_flight {
            let _ = respond.send(ControlResponse::error(
                "busy",
                "An agent is already being created or forked. Wait for it to finish, then retry split-out.",
            ));
            return;
        }
        if params.repo.is_none() && params.project.is_none() {
            let _ = respond.send(ControlResponse::error(
                "invalid_request",
                "split-out requires --repo <path> or --project <id|name>.",
            ));
            return;
        }
        if let Some(source_session_id) = params.source_session_id.as_deref()
            && !self
                .sessions
                .iter()
                .any(|session| session.id == source_session_id)
        {
            let _ = respond.send(ControlResponse::error(
                "invalid_source",
                format!(
                    "Source agent session \"{source_session_id}\" no longer exists. Start the split-out from a running dux agent."
                ),
            ));
            return;
        }

        let request_id = Uuid::new_v4().to_string();
        self.pending_control_responses
            .insert(request_id.clone(), respond);
        self.split_out_in_flight = true;
        self.split_out_create_started = false;

        let source_session = params
            .source_session_id
            .as_deref()
            .and_then(|id| self.sessions.iter().find(|session| session.id == id))
            .cloned();
        let source_label = source_session
            .as_ref()
            .map(|session| {
                session
                    .title
                    .clone()
                    .unwrap_or_else(|| session.branch_name.clone())
            })
            .unwrap_or_else(|| "agent".to_string());
        self.set_busy(format!(
            "Agent \"{source_label}\" asked dux to create related work. Resolving the target project..."
        ));

        let projects = self.projects.clone();
        let config = self.config.clone();
        let paths = self.paths.clone();
        let worker_tx = self.worker_tx.clone();
        thread::spawn(move || {
            super::workers::run_split_out_prepare_job(
                request_id,
                params,
                source_session,
                projects,
                config,
                paths,
                worker_tx,
            );
        });
    }

    pub(crate) fn handle_split_out_prepared(&mut self, plan: SplitOutPlan) {
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
            self.fail_split_out(
                &plan.request_id,
                "busy",
                "An agent is already being created or forked. Wait for it to finish, then retry split-out.",
            );
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
            "Creating a related worktree for agent \"{source_label}\" in project \"{}\"...",
            plan.project.name
        );
        self.set_busy(busy.clone());
        self.split_out_create_started = true;
        let request = CreateAgentRequest::SplitOut {
            request_id: plan.request_id,
            source_session_id: source_session.id.clone(),
            group_id: group.id,
            parent_session_id: source_session.id,
            project: plan.project,
            custom_name: plan.name,
            base_branch: plan.base_branch,
            provider: plan.provider,
            handoff_prompt: plan.prompt,
            project_added: plan.project_is_new,
        };
        if let Err(err) = self.dispatch_create_agent_request(request, busy) {
            self.split_out_in_flight = false;
            self.set_error(format!(
                "Could not start related worktree creation: {err:#}"
            ));
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
        let message = message.into();
        if self.split_out_create_started {
            self.create_agent_in_flight = false;
        }
        self.split_out_create_started = false;
        self.split_out_in_flight = false;
        self.pending_split_out = None;
        if let Some(respond) = self.pending_control_responses.remove(request_id) {
            let _ = respond.send(ControlResponse::error(code, message.clone()));
        }
        self.set_error(message);
        let target = self.selected_left_target();
        self.rebuild_left_items();
        if let Some(target) = target {
            self.select_left_target(&target);
        }
    }

    pub(crate) fn complete_split_out(&mut self, request_id: &str, response: ControlResponse) {
        self.split_out_in_flight = false;
        self.split_out_create_started = false;
        self.pending_split_out = None;
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
    }

    pub(crate) fn control_response_for_session(
        &self,
        request_id: &str,
        session: &AgentSession,
        project: &Project,
        project_added: bool,
    ) -> ControlResponse {
        let group = session
            .group_id
            .as_deref()
            .and_then(|id| self.groups.iter().find(|group| group.id == id));
        let mut response = ControlResponse::success().with_message(format!(
            "Created related agent \"{}\" in project \"{}\". It is linked under Related work.",
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
        let _ = request_id;
        response
    }

    pub(crate) fn related_group_label(&self, group: &AgentGroup) -> String {
        related_group_label_for(&self.sessions, group)
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

    pub(crate) fn group_for_session(&self, session: &AgentSession) -> Option<&AgentGroup> {
        session
            .group_id
            .as_deref()
            .and_then(|id| self.groups.iter().find(|group| group.id == id))
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
