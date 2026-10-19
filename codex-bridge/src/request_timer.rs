use std::collections::HashSet;

/// Local event-time estimates, including model waiting but not completed tool runs.
/// An ambiguous or unobserved model response is deliberately left unmeasured.
#[derive(Debug, Clone)]
pub(crate) struct RequestTimer {
    boundary_ms: Option<u64>,
    start_ms: Option<u64>,
    last_output_ms: Option<u64>,
    tool_completed_ms: Option<u64>,
    tools: HashSet<String>,
    model_items: HashSet<String>,
    retired_items: HashSet<String>,
    saw_tool: bool,
    ambiguous: bool,
}

impl Default for RequestTimer {
    fn default() -> Self {
        Self {
            boundary_ms: Some(0),
            start_ms: None,
            last_output_ms: None,
            tool_completed_ms: None,
            tools: HashSet::new(),
            model_items: HashSet::new(),
            retired_items: HashSet::new(),
            saw_tool: false,
            ambiguous: false,
        }
    }
}

impl RequestTimer {
    pub(crate) fn model_output(&mut self, id: &str, at_ms: u64) {
        if self.retired_items.contains(id) {
            return;
        }
        if self.last_output_ms.is_none() {
            if !self.tools.is_empty() {
                self.ambiguous = true;
            }
            self.start_ms = self.boundary_ms;
            self.tool_completed_ms = None;
        } else if self.tool_completed_ms.is_some_and(|at| at <= at_ms) {
            // Output spanning a tool completion without an intervening usage
            // event cannot safely be assigned to one model request.
            self.ambiguous = true;
        }
        self.model_items.insert(id.to_owned());
        self.last_output_ms = Some(at_ms);
    }

    pub(crate) fn item(&mut self, kind: &str, id: &str, completed: bool, at_ms: u64) {
        match kind {
            "agentMessage" | "reasoning" | "plan" => {
                if !completed || self.model_items.contains(id) {
                    self.model_output(id, at_ms);
                }
            }
            "commandExecution"
            | "fileChange"
            | "mcpToolCall"
            | "dynamicToolCall"
            | "collabAgentToolCall" => {
                if completed {
                    if !self.tools.remove(id) {
                        if self.retired_items.contains(id) {
                            return;
                        }
                        // A missing start can also mean missing intervening events.
                        if self.last_output_ms.is_some() {
                            self.ambiguous = true;
                        }
                    }
                    self.tool_completed_ms = Some(at_ms);
                    if self.tools.is_empty() {
                        self.boundary_ms = Some(at_ms);
                    }
                } else if !self.tools.contains(id) && !self.retired_items.contains(id) {
                    if self.tool_completed_ms.is_some() && self.last_output_ms.is_some() {
                        self.ambiguous = true;
                    }
                    // Tool execution starts after its model-generated call is ready.
                    if self.last_output_ms.is_none() {
                        self.model_output(id, at_ms);
                    } else {
                        self.model_items.insert(id.to_owned());
                        self.last_output_ms = Some(at_ms);
                    }
                    self.tools.insert(id.to_owned());
                    self.saw_tool = true;
                }
            }
            "userMessage" | "functionCallOutput" => {
                if self.last_output_ms.is_none() && self.tools.is_empty() {
                    self.boundary_ms = Some(at_ms);
                }
            }
            _ => self.invalidate(),
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.ambiguous = true;
        self.boundary_ms = None;
    }

    pub(crate) fn finish(&mut self, at_ms: u64) -> Option<u64> {
        let result = if !self.ambiguous {
            self.last_output_ms.and_then(|last| {
                if last > at_ms {
                    return None;
                }
                let end = if self.saw_tool { last } else { at_ms };
                let duration = end.checked_sub(self.start_ms?)?;
                (duration > 0 && end <= at_ms).then_some(duration)
            })
        } else {
            None
        };
        self.retired_items.extend(self.model_items.drain());
        self.start_ms = None;
        self.last_output_ms = None;
        self.tool_completed_ms = None;
        self.saw_tool = false;
        self.ambiguous = false;
        self.boundary_ms = self.tools.is_empty().then_some(at_ms);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_waiting_and_measures_each_response_separately() {
        let mut timer = RequestTimer::default();
        timer.item("reasoning", "r1", false, 2_000);
        timer.item("reasoning", "r1", true, 5_000);
        timer.model_output("m1", 7_000);
        assert_eq!(timer.finish(10_000), Some(10_000));
        timer.model_output("m2", 12_000);
        assert_eq!(timer.finish(15_000), Some(5_000));
    }

    #[test]
    fn parallel_tool_wait_is_not_charged_to_either_request() {
        let mut timer = RequestTimer::default();
        timer.item("commandExecution", "t1", false, 5_000);
        timer.item("mcpToolCall", "t2", false, 6_000);
        assert_eq!(timer.finish(6_100), Some(6_000));
        timer.item("commandExecution", "t1", true, 50_000);
        timer.item("mcpToolCall", "t2", true, 60_000);
        timer.model_output("m2", 62_000);
        assert_eq!(timer.finish(70_000), Some(10_000));
    }

    #[test]
    fn usage_delayed_until_after_tool_completion_uses_model_end() {
        let mut timer = RequestTimer::default();
        timer.item("dynamicToolCall", "t1", false, 5_000);
        timer.item("dynamicToolCall", "t1", true, 55_000);
        assert_eq!(timer.finish(55_010), Some(5_000));
        timer.model_output("m2", 58_000);
        assert_eq!(timer.finish(60_010), Some(5_000));
    }

    #[test]
    fn missing_or_ambiguous_output_is_not_estimated() {
        let mut timer = RequestTimer::default();
        assert_eq!(timer.finish(20_000), None);
        timer.item("contextCompaction", "c1", false, 21_000);
        timer.model_output("m1", 25_000);
        assert_eq!(timer.finish(30_000), None);
        timer.item("commandExecution", "t1", false, 35_000);
        timer.item("commandExecution", "t1", true, 60_000);
        timer.model_output("m2", 65_000);
        assert_eq!(timer.finish(70_000), None);
    }

    #[test]
    fn late_completed_items_cannot_start_the_next_request() {
        let mut timer = RequestTimer::default();
        timer.model_output("m1", 2_000);
        assert_eq!(timer.finish(3_000), Some(3_000));
        timer.item("agentMessage", "m1", true, 3_010);
        timer.model_output("m1", 3_020);
        assert_eq!(timer.finish(20_000), None);
    }

    #[test]
    fn repeated_tool_completion_does_not_move_the_next_boundary() {
        let mut timer = RequestTimer::default();
        timer.item("commandExecution", "t1", false, 1_000);
        timer.finish(1_100);
        timer.item("commandExecution", "t1", true, 10_000);
        timer.item("commandExecution", "t1", true, 15_000);
        timer.model_output("m2", 18_000);
        assert_eq!(timer.finish(20_000), Some(10_000));
    }

    #[test]
    fn reversed_time_and_output_while_tools_are_running_are_rejected() {
        let mut timer = RequestTimer::default();
        timer.model_output("m1", 5_000);
        assert_eq!(timer.finish(4_000), None);
        timer.item("commandExecution", "t1", false, 10_000);
        timer.finish(11_000);
        timer.model_output("m2", 12_000);
        assert_eq!(timer.finish(15_000), None);
    }
}
