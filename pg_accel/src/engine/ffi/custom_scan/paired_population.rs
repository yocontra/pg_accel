#[derive(Clone, Debug, serde::Serialize)]
pub struct V2Leaf {
    pub whole: u64,
    pub partition: u64,
    pub input: u64,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct PopulationTrace {
    pub actual_v2_calls: u64,
    pub leaves: Vec<V2Leaf>,
    pub capacities: Vec<usize>,
}
thread_local! {
    static CAPTURE: std::cell::RefCell<Option<PopulationTrace>> = const {
        std::cell::RefCell::new(None)
    };
}
pub fn record_v2_call(capacity: usize) {
    CAPTURE.with(|slot| {
        if let Some(trace) = slot.borrow_mut().as_mut() {
            trace.actual_v2_calls = trace
                .actual_v2_calls
                .checked_add(1)
                .expect("bounded diagnostic counter");
            trace.capacities.push(capacity);
        }
    });
}
pub fn record_successful_leaf(whole: u64, partition: u64, input: u64) {
    CAPTURE.with(|slot| {
        if let Some(trace) = slot.borrow_mut().as_mut() {
            trace.leaves.push(V2Leaf {
                whole,
                partition,
                input,
            });
        }
    });
}
pub fn capture<T>(body: impl FnOnce() -> T) -> (T, PopulationTrace) {
    struct Restore(Option<PopulationTrace>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CAPTURE.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore =
        Restore(CAPTURE.with(|slot| slot.borrow_mut().replace(PopulationTrace::default())));
    let result = body();
    let trace = CAPTURE
        .with(|slot| slot.borrow_mut().take())
        .expect("active trace");
    (result, trace)
}

// Module must be included under #[cfg(feature="pg_test")] only.
// Call record_v2_call immediately before actual v2 bridge invocation in
// grouped_agg.rs (not at a forecast/shape check). Failed calls count as calls,
// not successful populations. Record leaf ONLY at descriptor.rs after
// driver.complete_population_leaf(selected,whole,input) returns Ok and only
// if validated SQL WHERE is present (v1 no-WHERE must not be mislabeled v2).
// Natural one-shot completion is deliberately captured; EXPLAIN overflow-only
// visibility is irrelevant. Separate aggregate verified/scanned counters and
// mask lifecycle counters still require exact binding in the Backend adapter.
