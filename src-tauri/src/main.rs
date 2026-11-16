// Keeps the console window from appearing behind the GUI on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    fulcrum_lib::run()
}
