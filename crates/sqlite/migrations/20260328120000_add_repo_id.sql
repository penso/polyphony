-- Add repo_id column to tables for multi-repository support.
-- Defaults to empty string for backward compatibility with existing rows.

alter table agent_run_records add column repo_id text not null default '';
alter table runs add column repo_id text not null default '';
alter table tasks add column repo_id text not null default '';
alter table reviewed_pull_request_heads add column repo_id text not null default '';
