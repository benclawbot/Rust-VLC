fn main() {
    println!("cargo:rerun-if-changed=ui/app.slint");
    let configuration = slint_build::CompilerConfiguration::default().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", configuration)
        .expect("failed to compile the Slint UI");
}
