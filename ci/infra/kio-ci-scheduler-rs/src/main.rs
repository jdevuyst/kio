fn main() {
    match kio_ci_scheduler::main_entry(std::env::args_os()) {
        Ok(status) => std::process::exit(status),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    }
}
