-- The label of the ERC-4626 vault a yield asset earns in.
--
-- A yield asset shares its token with the plain asset of the same underlying,
-- so `assets.symbol` alone names both the same. The vault's own `name()` is
-- what tells them apart.
--
-- Read from chain rather than configured: the indexer resolves
-- `IYieldVenue(venue).VAULT()` and then that vault's `name()`, both immutable
-- in practice, so one successful read is final. NULL until the metadata sweep
-- has read it, or permanently for a vault that does not implement `name()`.
--
-- Not part of the event-sourced binding, so replaying `YieldAssetAdded` after a
-- cursor rewind never clears it.
ALTER TABLE asset_yield
    ADD COLUMN vault_name TEXT;
