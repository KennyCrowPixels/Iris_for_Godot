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
            commands::close_tab_and_snapshot,
            commands::restore_last_closed_tab,
            commands::read_tab_snapshot,
            commands::test_model,
            commands::create_tab_memory,
            ensure_coder_lite_model,
            commands::get_universal_prompts,
            commands::get_tab_context,
            commands::close_window,
            commands::list_open_tabs,
            commands::restore_full_tab_memory,
            ensure_model,
            commands::update_snapshot_memory,
            commands::debug_memory_dir,
            commands::debug_list_open_tab_files,
            commands::seed_open_tabs_from_dev_dir,
            commands::show_devtools,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}


