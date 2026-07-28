fn main() {
    match netsandbox::run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("netsandbox: {error:#}");
            std::process::exit(1);
        }
    }
}
