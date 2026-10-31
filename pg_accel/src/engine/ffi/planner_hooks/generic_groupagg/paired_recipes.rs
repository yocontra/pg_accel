use super::{AggOutputProjection, ShapePlan};
#[cfg(feature = "pg_test")]
#[derive(Clone)]
pub struct PairedRecipe {
    pub(crate) selection: crate::engine::executor::agg::AggExecutionSelection,
    pub(crate) spec: crate::engine::spec::AggQuerySpec,
    pub(crate) projection: AggOutputProjection,
    pub(crate) cost_components: String,
}

#[cfg(feature = "pg_test")]
thread_local! {
    static PAIRED_RECIPES: std::cell::RefCell<Option<Vec<PairedRecipe>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(feature = "pg_test")]
pub(super) fn paired_capture_recipe(shape: &ShapePlan) {
    if !PAIRED_RECIPES.with(|slot| slot.borrow().is_some()) {
        return;
    }
    let recipe = PairedRecipe {
        selection: shape.execution_selection(),
        spec: shape.spec.clone(),
        projection: AggOutputProjection {
            slots: shape.projections.clone(),
        },
        cost_components: format!("{:?}", shape.cost),
    };
    PAIRED_RECIPES.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .expect("active diagnostic")
            .push(recipe);
    });
}

#[cfg(feature = "pg_test")]
pub fn capture_paired_recipes<T>(
    body: impl FnOnce() -> T,
) -> Result<(T, [PairedRecipe; 2]), String> {
    struct Restore(Option<Vec<PairedRecipe>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PAIRED_RECIPES.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(PAIRED_RECIPES.with(|slot| slot.borrow_mut().replace(Vec::new())));
    let value = body(); // guarded SPI EXPLAIN without ANALYZE; no TLS borrow held
    let recipes = PAIRED_RECIPES
        .with(|slot| slot.borrow_mut().take())
        .ok_or("diagnostic capture was unexpectedly cleared")?;
    let [legacy, raw]: [PairedRecipe; 2] = recipes
        .try_into()
        .map_err(|_| "need exactly two independently built candidates")?;
    use crate::engine::executor::agg::AggExecutionSelection;
    if legacy.selection != AggExecutionSelection::Legacy
        || raw.selection != AggExecutionSelection::RawIntegerCount
    {
        return Err("unexpected candidate order/selection".into());
    }
    Ok((value, [legacy, raw]))
}

// Exact hook site: immediately after apply_exact_residency returns Ok in
// try_inject_candidate, before effective_path_cost/add_native_having_cost/gate.
// Add `#[cfg(feature="pg_test")] paired_capture_recipe(&shape);`.
// Re-export capture_paired_recipes/PairedRecipe from planner_hooks/mod.rs ONLY
// under pg_test. Early cache hits intentionally yield missing recipes and stop
// the diagnostic; do not clear caches or manufacture the missing candidate.
