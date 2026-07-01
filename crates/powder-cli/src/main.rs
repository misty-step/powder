fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match powder_cli::run(&args) {
        Ok(output) => print!("{output}"),
        Err(err) => {
            eprintln!("powder: {err}");
            std::process::exit(1);
        }
    }
}
