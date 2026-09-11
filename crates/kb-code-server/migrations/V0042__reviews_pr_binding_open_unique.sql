-- V76-R1b — unique (repo, pr_number) only among OPEN reviews.
--
-- `start-pr --new` (POST /api/reviews/pr?on_closed=new) mints a new review
-- id for a (repo, PR) whose existing review is CLOSED. The closed row keeps
-- its pr_number (history / refs list still attribute the PR ref to it until
-- the new open review exists; both may share the number). V0024's unique
-- index `idx_reviews_pr_binding` was `WHERE pr_number IS NOT NULL` with no
-- state predicate, so a second row with the same (repo, pr_number) could
-- never land. Drop and recreate as OPEN-only.
--
-- Two OPEN reviews still cannot share a PR. Two CLOSED reviews can.
-- Reopening a closed review while another OPEN row already holds that
-- binding hits this index and is a 409, not a silent overwrite.

DROP INDEX IF EXISTS idx_reviews_pr_binding;
CREATE UNIQUE INDEX idx_reviews_pr_binding ON reviews(repo, pr_number)
    WHERE pr_number IS NOT NULL AND state = 'open';
