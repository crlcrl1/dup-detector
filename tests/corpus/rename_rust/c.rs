struct Report {
    title: String,
    rows: Vec<String>,
}

impl Report {
    fn render(&self) -> String {
        let mut out = self.title.clone();
        for row in &self.rows {
            out.push('\n');
            out.push_str(row);
        }
        out
    }
}
