-- How many times a member has confirmed a provider-side reauthorization of
-- the connection. Plaid's update mode keeps the credentials as they are, so a
-- run compares this with what it read when it started to tell whether what
-- the provider told it is out of date.
ALTER TABLE connections ADD COLUMN reauth_generation INTEGER NOT NULL DEFAULT 0;
