fn main() {
    if let Err(error) = ntlmrain::run() {
        eprintln!("error: {error:#}");
        std::process::exit(ntlmrain::error_exit_code(&error));
    }
}
