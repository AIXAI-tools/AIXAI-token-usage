// 發行版不開主控台視窗
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    aixai_token_usage_lib::run()
}
