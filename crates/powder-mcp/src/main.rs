fn main() {
    for tool in powder_mcp::tools() {
        println!("{}\t{}", tool.name, tool.description);
    }
}
