-- Optional employee attributes used by the compensation benchmark.
ALTER TABLE employees ADD COLUMN IF NOT EXISTS education_level VARCHAR(20)
    CHECK (education_level IN ('high_school','bachelor','master','phd'));
-- Total professional experience in whole years as of the hire date (stays valid as time passes).
ALTER TABLE employees ADD COLUMN IF NOT EXISTS years_experience INT
    CHECK (years_experience BETWEEN 0 AND 60);
-- Cost-of-living tier of the work location: 1 (highest cost) to 3 (lowest).
ALTER TABLE employees ADD COLUMN IF NOT EXISTS city_tier SMALLINT
    CHECK (city_tier BETWEEN 1 AND 3);
ALTER TABLE employees ADD COLUMN IF NOT EXISTS remote_work BOOLEAN;
