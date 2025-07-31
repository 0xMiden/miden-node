use std::{
    fmt::{Display, Formatter},
    ops::Not,
};

pub mod renderer;

impl Transaction<'_> {
    /// Panics if the query plan contains a slow table scan.
    pub fn check_query_plan(&self, sql: &str) {
        let query_plan = self.query_plan(sql).expect("Must be a valid SQL");

        assert!(
            query_plan.contains_unnecessary_scans().not(),
            "Query plan contains unnecessary table scan(s):\n{query_plan}"
        );
    }

    /// Renders query plan as tree using ASCII graphics.
    fn query_plan(&self, sql: &str) -> rusqlite::Result<QueryPlan> {
        let explain_sql = format!("EXPLAIN QUERY PLAN {sql}");
        let explain_stmt = self.prepare(&explain_sql)?;
        let query_plan = QueryPlanRenderer::new().render_tree(explain_stmt)?;

        Ok(QueryPlan { explain_sql, query_plan })
    }
}

/// Represents rendered query plan with `EXPLAIN QUERY PLAN ...` SQL query.
struct QueryPlan {
    explain_sql: String,
    query_plan: String,
}

impl Display for QueryPlan {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}\n\n{}", self.explain_sql, self.query_plan)
    }
}

impl QueryPlan {
    /// Checks if the given query plan contains unnecessary table scans not utilizing indices.
    fn contains_unnecessary_scans(&self) -> bool {
        use std::sync::LazyLock;

        static RE_IDX_SCAN: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::RegexBuilder::new(
                r"SCAN ([A-Za-z0-9_]+) (USING( COVERING)?|VIRTUAL TABLE) INDEX ([A-Za-z0-9_]+)",
            )
            .case_insensitive(true)
            .build()
            .expect("Must be a valid regex pattern")
        });

        // Some index scans might cause false positiveness if we just look for `SCAN` keyword:
        // ```
        //   SCAN rarray VIRTUAL TABLE INDEX 1
        //   SCAN table1 USING INDEX sqlite_autoindex_table1_1
        //   SCAN table1 USING COVERING INDEX idx_table1_column
        // ```
        // Preprocess query plan in order to replace such cases to `SEARCH` expressions instead of
        // `SCAN`. Another solution would be to find only scan expressions which don't accompanied
        // by known index suffixes, but current `regex` implementation doesn't support look-around.
        let query_plan = RE_IDX_SCAN.replace_all(&self.query_plan, r"SEARCH $1 $2 INDEX $4");

        // Don't flag `SELECT` queries without `WHERE` clause, since they don't usually use indexes
        if self.explain_sql.contains("SELECT") && !self.explain_sql.contains("WHERE") {
            return false;
        }

        query_plan.contains(" SCAN ")
    }
}
