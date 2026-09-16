use crate::language::LanguageId;

#[derive(Debug, Clone)]
pub struct Config {
    pub min_lines: usize,
    pub min_occurrences: usize,
    pub max_bucket: usize,
    pub seed_window: usize,
    pub max_groups: Option<usize>,
    pub parameterize_literals: bool,
    pub languages: Vec<LanguageId>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_lines: 4,
            min_occurrences: 2,
            max_bucket: 32,
            seed_window: 8,
            max_groups: None,
            parameterize_literals: false,
            languages: LanguageId::ALL.to_vec(),
        }
    }
}

impl Config {
    pub fn language_enabled(&self, language: LanguageId) -> bool {
        self.languages.contains(&language)
    }

    pub fn with_limits(
        mut self,
        min_lines: Option<usize>,
        min_occurrences: Option<usize>,
        max_groups: Option<usize>,
    ) -> Self {
        if let Some(v) = min_lines {
            self.min_lines = v;
        }
        if let Some(v) = min_occurrences {
            self.min_occurrences = v;
        }
        if let Some(v) = max_groups {
            self.max_groups = Some(v);
        }
        self
    }
}
