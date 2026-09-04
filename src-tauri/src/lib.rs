pub mod agents;
pub mod antigravity;
pub mod cache;
pub mod chat;
pub mod claudecfg;
pub mod detail;
pub mod diag;
pub mod fonts;
pub mod fsx;
pub mod git;
pub mod grok;
pub mod hooklink;
pub mod indexer;
pub mod launch;
pub mod librarian;
pub mod mcp;
pub mod notify;
pub mod opencode;
pub mod opencode_agent;
pub mod permissions;
pub mod providers;
pub mod pty;
pub mod remote;
pub mod remote_api;
pub mod remote_roads;
pub mod iroh_tunnel;
pub mod changes;
pub mod rendercost;
pub mod services;
pub mod sessions;
pub mod spine;
pub mod tabs;
pub mod taskbar;
pub mod terminal;
pub mod trace;
pub mod tray;
pub mod usage;
pub mod watcher;
pub mod winstate;

/// Run a blocking body on the async runtime's blocking pool. Tauri executes
/// non-async commands on the GTK main thread, where every millisecond is a
/// frame — a command routed through here costs the main loop nothing.
pub async fn run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .expect("blocking command panicked")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    trace::init();
    let pty = pty::PtyManager::default();
    let tabs = std::sync::Arc::new(tabs::TabRegistry::new(pty.clone()));
    // The spine's epoch is set the moment this is built: a phone that sees a
    // new one knows the desktop restarted and its seq numbers started over.
    let spine = std::sync::Arc::new(spine::Spine::new());
    let application_services = services::ApplicationServices::default();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(pty)
        .manage(tabs.clone())
        .manage(spine)
        .manage(application_services)
        .manage(changes::ChangeLedger::default())
        .manage(watcher::WatchState::default())
        // Off until the user turns it on, and it opens nothing on disk until
        // then: a desktop that never pairs a phone never grows a
        // trusted-device file.
        .manage(remote::RemoteState::default())
        .manage(remote_api::RemoteState::default())
        // Wrapped so a debug build logs every IPC call before it dispatches.
        // In release `log_invokes` is the identity function and the generated
        // handler is passed straight through — see `trace.rs`.
        .invoke_handler(trace::log_invokes(tauri::generate_handler![
            tabs::tab_open,
            tabs::tab_list,
            tabs::tab_registry_snapshot,
            tabs::tab_update,
            tabs::tab_attach_desktop,
            tabs::tab_detach,
            tabs::tab_write,
            tabs::tab_resize,
            tabs::tab_take_focus,
            tabs::tab_close,
            librarian::librarian_state,
            librarian::librarian_run,
            librarian::librarian_pending,
            librarian::librarian_forget,
            detail::session_conversation,
            detail::session_transcript_path,
            changes::session_changes,
            changes::read_file_base64,
            agents::detect_agents,
            agents::agent_caps,
            rendercost::renderer_probe,
            claudecfg::claude_settings,
            claudecfg::claude_save_layer,
            claudecfg::claude_set_key,
            claudecfg::claude_instructions,
            claudecfg::claude_mcp,
            claudecfg::claude_skills,
            claudecfg::claude_hooks,
            taskbar::taskbar_badge,
            notify::desktop_notify,
            notify::desktop_notify_close,
            tray::tray_alerts,
            agents::adopt_agent_session,
            diag::diag_log_path,
            diag::diag_log_tail,
            diag::diag_environment,
            agents::agent_choices,
            agents::clear_successor_session,
            launch::resolve_launch,
            permissions::agent_permissions,
            permissions::agent_permission_set,
            providers::providers_list,
            providers::provider_save,
            providers::provider_delete,
            providers::provider_models,
            providers::provider_model_cards,
            providers::provider_model_endpoints,
            providers::provider_startup_set,
            providers::provider_directory,
            providers::provider_policy_set,
            providers::provider_route_set,
            providers::provider_activity,
            providers::provider_management_key_set,
            sessions::list_sessions,
            sessions::session_rename,
            sessions::session_titles,
            sessions::session_stars,
            sessions::session_brought_in,
            sessions::session_star,
            sessions::session_status,
            sessions::session_preview,
            detail::session_detail,
            sessions::session_delete,
            sessions::trash_list,
            sessions::trash_restore,
            sessions::trash_delete,
            sessions::trash_empty,
            sessions::session_tasks,
            sessions::session_agents,
            sessions::session_artifacts,
            sessions::running_session_ids,
            sessions::bg_agent_session_ids,
            sessions::unstoppable_session_ids,
            sessions::session_moved_to,
            sessions::ui_log,
            sessions::live_session_ids,
            sessions::stop_session,
            sessions::resolve_resumable_id,
            sessions::session_fork,
            sessions::materialize_fork,
            sessions::claude_permission_mode,
            sessions::claude_model_default,
            sessions::restore_claude_model_default,
            sessions::session_model,
            sessions::session_refusal,
            opencode_agent::opencode_dispatch,
            opencode_agent::opencode_default_target,
            hooklink::drain_session_events,
            spine::ipc::spine_overview,
            trace::trace_set,
            trace::trace_status,
            usage::usage_report,
            fonts::list_fonts,
            fonts::font_packages,
            fonts::install_font_package,
            fonts::install_font_files,
            watcher::watch_project,
            fsx::list_dir,
            fsx::open_path,
            fsx::list_projects,
            fsx::read_text_file,
            fsx::write_text_file,
            indexer::reindex_sessions,
            indexer::search_sessions,
            git::git_repo_state,
            git::git_status,
            git::git_branches,
            git::git_branch_files,
            git::git_branch_log,
            git::git_log,
            git::git_diff_file,
            git::git_commit_diff,
            remote::remote_status,
            remote::remote_interfaces,
            remote::remote_start,
            remote::remote_stop,
            remote::remote_relay_configure,
            remote::remote_relay_clear,
            remote::remote_begin_pairing,
            remote::remote_begin_pairing_combined,
            remote::remote_pending_pairings,
            remote::remote_approve_device,
            remote::remote_deny_device,
            remote::remote_devices,
            remote::remote_revoke_device,
            remote_api::remote_api_status,
            remote_api::remote_set_enabled,
            remote_api::remote_rotate_token,
            remote_api::remote_set_name,
            remote_api::remote_set_iroh,
            remote_api::remote_set_road,
            remote_api::remote_set_iroh_relay_url,
            remote_api::remote_set_upnp,
            remote_api::remote_phone_relay_clear,
            remote_api::remote_set_road_order,
            remote_api::remote_set_port,
            remote_api::remote_pair_payload,
            remote_api::relay_report,
        ]))
        .setup(move |app| {
            if let Err(e) =
                crate::tabs::start_desktop_registry_bridge(app.handle().clone(), tabs.clone())
            {
                crate::diag!("tabs", "desktop registry bridge not running: {e}");
            }
            // First line of every log: which build this is and what launched
            // it. The crash that took an hour to pin down last night was an
            // aiterm started from inside another one's process tree, and the
            // parent pid is the whole of that story.
            crate::diag!(
                "start",
                "aiterm {} pid={} ppid={}",
                env!("CARGO_PKG_VERSION"),
                std::process::id(),
                std::fs::read_to_string("/proc/self/status")
                    .ok()
                    .and_then(|s| s
                        .lines()
                        .find_map(|l| l.strip_prefix("PPid:").map(|v| v.trim().to_string())))
                    .unwrap_or_else(|| "?".into())
            );

            // The settings file claude launches load their hooks from.
            // Every launch, because it embeds this binary's path.
            hooklink::install();

            // And the app side of the phase hooks: an inotify watch on their
            // spool, so a permission dialog reaches the phone as it opens
            // rather than on the next poll of something.
            hooklink::start_hook_drain(app.handle().clone());

            // Push sessions-list refreshes when an agent's transcripts change
            // (new/cleared/forked sessions) instead of waiting for the 30s poll.
            if let Err(e) = watcher::watch_claude_projects(app.handle().clone()) {
                crate::diag!("start", "transcript watcher not running: {e}");
            }

            // Track files produced by active sessions. The structured mobile
            // API reads this ledger; it does not replace tab/session ownership.
            changes::start(app.handle());

            // The phone-protocol listener (with its iroh tunnel), separate
            // from the remote gateway above and off unless enabled in
            // Settings. A phone paired earlier expects the desktop to answer
            // again.
            remote_api::autostart(app.handle());

            // Ask for the saved size less whatever this desktop's decorations
            // add to it. Runs after the plugin's own restore, so it wins.
            // The tray is where a waiting session can be reached from while
            // aiterm is behind something. Failing to create one is not worth
            // refusing to start over.
            if let Err(e) = tray::init(app.handle()) {
                crate::diag!("tray", "could not create tray icon: {e}");
            }
            winstate::correct_restored_size(app.handle());
            // Then measure what actually landed, once the compositor has
            // settled the surface, and remember it for next launch.
            {
                let h = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(2500));
                    let inner = h.clone();
                    let _ = h.run_on_main_thread(move || winstate::learn_drift(&inner));
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
