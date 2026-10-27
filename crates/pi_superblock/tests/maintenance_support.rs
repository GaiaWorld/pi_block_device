use std::path::PathBuf;
use tempfile::TempDir;

pub struct IgnoreVisitor;

impl pi_append_log::AppendLogVisitor for IgnoreVisitor {
    fn visit(
        &mut self,
        _block: &[u8],
        _context: pi_append_log::BlockVisitContext,
    ) -> pi_result::Result<bool> {
        Ok(false)
    }
}

pub fn log_path(temp: &TempDir, name: &str) -> PathBuf {
    temp.path().join(name)
}
