//! The prices a model declares, in the shape `loop` prices with.

/// Field-by-field copy of a model's declared prices. `loop` cannot depend on
/// `config`, so the prices it prices with live in `contract`.
pub(crate) fn declared(cost: config::Cost) -> contract::provider::Cost {
    contract::provider::Cost {
        input: cost.input,
        output: cost.output,
        cache_read: cost.cache_read,
        cache_write: cost.cache_write,
        tiers: cost
            .tiers
            .into_iter()
            .map(|tier| contract::provider::Tier {
                input_tokens_above: tier.input_tokens_above,
                input: tier.input,
                output: tier.output,
                cache_read: tier.cache_read,
                cache_write: tier.cache_write,
            })
            .collect(),
    }
}
