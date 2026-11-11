-- The cap of characters of one account lives in the table, so no client and no copy of the login service
-- passes it. The trigger holds the row of the account while it counts, so two inserts of one account
-- count one after the other. The cap is the same as CHARACTERS in the crate mmo-store.

CREATE FUNCTION characters_account_cap() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM 1 FROM accounts WHERE id = NEW.account_id FOR NO KEY UPDATE;
  IF (SELECT count(*) FROM characters WHERE account_id = NEW.account_id) >= 8 THEN
    RAISE EXCEPTION 'the account % holds 8 characters already', NEW.account_id
      USING ERRCODE = 'check_violation', CONSTRAINT = 'characters_account_cap';
  END IF;
  RETURN NEW;
END;
$$;

CREATE TRIGGER characters_account_cap BEFORE INSERT ON characters
  FOR EACH ROW EXECUTE FUNCTION characters_account_cap();
