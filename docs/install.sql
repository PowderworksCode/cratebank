INSTALL httpfs;
LOAD httpfs;

CREATE OR REPLACE VIEW sessions AS
SELECT * FROM read_parquet('https://data.cratebank.io/sessions.parquet');

CREATE OR REPLACE VIEW units AS
SELECT * FROM read_parquet('https://data.cratebank.io/units.parquet');

CREATE OR REPLACE VIEW phases AS
SELECT * FROM read_parquet('https://data.cratebank.io/phases.parquet');

CREATE OR REPLACE VIEW timeline AS
SELECT * FROM read_parquet('https://data.cratebank.io/timeline.parquet');

CREATE OR REPLACE VIEW unit_flags AS
SELECT * FROM read_parquet('https://data.cratebank.io/unit_flags.parquet');
