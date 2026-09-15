use crate::language::LanguageId;

#[derive(Debug, Clone)]
pub struct Config {
    pub min_tokens: usize,
    pub min_occurrences: usize,
    pub max_bucket: usize,
    pub seed_window: usize,
    pub max_groups: usize,
    pub parameterize_literals: bool,
    pub languages: Vec<LanguageId>,
    pub type3: bool,
    pub type3_max_gap: usize,
    pub type3_min_run: usize,
    pub type3_min_similarity: f64,
    pub type3_max_lcs_span: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_tokens: 40,
            min_occurrences: 2,
            max_bucket: 32,
            seed_window: 8,
            max_groups: 50,
            parameterize_literals: false,
            languages: LanguageId::ALL.to_vec(),
            type3: false,
            type3_max_gap: 12,
            type3_min_run: 4,
            type3_min_similarity: 0.7,
            type3_max_lcs_span: 1500,
        }
    }
}

impl Config {
    pub fn language_enabled(&self, language: LanguageId) -> bool {
        self.languages.contains(&language)
    }

    pub fn with_limits(
        mut self,
        min_tokens: Option<usize>,
        min_occurrences: Option<usize>,
        max_groups: Option<usize>,
    ) -> Self {
        if let Some(v) = min_tokens {
            self.min_tokens = v;
        }
        if let Some(v) = min_occurrences {
            self.min_occurrences = v;
        }
        if let Some(v) = max_groups {
            self.max_groups = v;
        }
        self
    }
}
