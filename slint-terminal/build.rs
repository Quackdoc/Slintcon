fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .as_library("SlintTerminal")
        .rust_module("ui");
    slint_build::compile_with_config("ui/terminal.slint", config)
        .expect("failed to compile the terminal widget");
}
