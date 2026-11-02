-- A full rebuild runs in short transactions (issue #83). This is the last account
-- it has finished, in id order: rows up to it are new and changes keep them current,
-- and later ones wait their turn. NULL means no batch has run, so a rebuild can
-- resume after a crash and start over when what it is computing has changed.
ALTER TABLE net_worth_state
    ADD COLUMN rebuilt_up_to TEXT CHECK (rebuilt_up_to IS NULL OR rebuild_needed = 1);
