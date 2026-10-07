ALTER TABLE IF EXISTS instances ADD column phone_home_enabled BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE IF EXISTS instances ADD column phone_home_last_contact TIMESTAMPTZ NULL;
