create table if not exists runtime_snapshots (
  generated_at text not null,
  payload text not null
);

create table if not exists run_records (
  id integer primary key autoincrement,
  issue_id text not null,
  issue_identifier text not null,
  session_id text,
  status text not null,
  attempt integer,
  started_at text not null,
  finished_at text,
  payload text not null
);

create table if not exists budget_snapshots (
  id integer primary key autoincrement,
  component text not null,
  captured_at text not null,
  payload text not null
);

create table if not exists movements (
  id integer primary key autoincrement,
  movement_id text not null unique,
  issue_id text,
  status text not null,
  created_at text not null,
  updated_at text not null,
  payload text not null
);

create table if not exists tasks (
  id integer primary key autoincrement,
  task_id text not null unique,
  movement_id text not null,
  status text not null,
  ordinal integer not null,
  created_at text not null,
  updated_at text not null,
  payload text not null
);

create table if not exists reviewed_pull_request_heads (
  id integer primary key autoincrement,
  review_key text not null unique,
  reviewed_at text not null,
  payload text not null
);
