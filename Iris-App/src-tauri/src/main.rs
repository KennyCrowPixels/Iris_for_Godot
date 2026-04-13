// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use crate::commands::ensure_model;
mod commands;
use commands::ensure_coder_lite_model;
use crate::commands::ensure_ollama_running_once;

fn main() {
    tauri::Builder::default()
        .setup(|_app| {
            ensure_ollama_running_once();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::restore_full_tab_memory,
            commands::update_tab_memory,
            commands::get_compiled_context,
            commands::interpret_turn_v2,
            commands::close_tab_and_snapshot,
            commands::restore_last_closed_tab,
            commands::clear_last_closed_tab,
            commands::read_tab_snapshot,
            commands::test_model,
            commands::create_tab_memory,
            ensure_coder_lite_model,
            commands::get_universal_prompts,
            commands::get_tab_context,
            commands::close_window,
            commands::list_open_tabs,
            ensure_model,
            commands::update_snapshot_memory,
            commands::debug_memory_dir,
            commands::debug_list_open_tab_files,
            commands::seed_open_tabs_from_dev_dir,
            commands::show_devtools,
            commands::sanitize_tab_titles,
            commands::check_ollama_installed,
            commands::get_setup_flags,
            commands::set_setup_flags,
            commands::download_and_install_ollama,
            commands::check_models_ready,
            commands::apply_model_profile,
            commands::pull_and_create_models,
            commands::restart_app,
            commands::open_modelfiles_folder,
            commands::open_user_persona_prompt,
            commands::get_core_persona_prompt,
            commands::get_user_persona_prompt,
            commands::save_user_persona_prompt,
            commands::pick_repo_folder,
            commands::scan_repo_entries,
            commands::read_repo_entry_excerpt,
            commands::fs_list_dir,
            commands::fs_read_text,
            commands::fs_write_text,
            commands::fs_delete_path,
            commands::fs_move_path,
            commands::fs_make_dir,
            commands::clone_repo_into_folder,
            commands::install_repo_dependencies,
            commands::network_lookup,
            commands::network_search,
            commands::weather_lookup,
            commands::get_repo_project_store,
            commands::save_repo_project_store,
            commands::capture_project_checkpoint,
            commands::list_project_checkpoints,
            commands::restore_project_checkpoint,
            commands::read_project_dataweb,
            commands::write_project_dataweb,
            commands::open_project_dataweb,
            commands::read_universal_dataweb,
            commands::write_universal_dataweb,
            commands::open_universal_dataweb,
            commands::detect_hardware_profile,
                    commands::list_modelfiles,
                    commands::read_modelfile_data,
                    commands::save_modelfile_data,
                    commands::get_model_config,
                    commands::save_model_config,
                    commands::create_custom_modelfile,
                    commands::delete_custom_modelfile,
                    commands::list_windows,
                    commands::get_system_stats,
                    commands::take_screenshot,
                    commands::take_window_screenshot,
                    commands::launch_mcp_server,
                    commands::stop_mcp_server,
                    commands::connect_mcp_server,
                    commands::mcp_list_tools,
                    commands::mcp_call_tool,
                    commands::ssh_tool_call,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}


