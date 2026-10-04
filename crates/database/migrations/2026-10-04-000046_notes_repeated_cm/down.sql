-- Fails while `notes` holds a repeated commitment. That is intended: the binary
-- this rolls back to cannot store one, so the rows have to be dealt with first.
ALTER TABLE notes ADD CONSTRAINT notes_chain_id_cm_key UNIQUE (chain_id, cm);
