struct Registry {
    entries: Vec<String>,
    lookup: std::collections::HashMap<String, usize>,
}

impl Registry {
    fn insert(&mut self, name: String) {
        if !self.lookup.contains_key(&name) {
            self.lookup.insert(name.clone(), self.entries.len());
            self.entries.push(name);
        }
    }
}
