fn main() {
    if let Err(error) = oxicale::cli::run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
