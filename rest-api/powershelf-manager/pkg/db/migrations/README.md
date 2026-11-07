Migrations are in SQL files of the format TIMESTAMP_NAME.up.sql and TIMESTAMP_NAME.down.sql.  Names do not necessarily need to be unique, but timestamps do.
The .up.sql is done for schema upgrades, and .down.sql for when rolling back a schema upgrade.  If it is unsafe to roll back, create a .down.sql file containing
text to describe the reason that is not valid SQL.

Once a migration has been used on a real site, it should in general NEVER be changed so that we can maintain a consistent environment.  If an exception needs to be made include a comment "-- Allow hash changing" to ignore this.
All sites where it was deployed will require manual schema editing to be consistent with the new schema unless the change was to add an UPDATE or DELETE command to modify the data instead of the schema.

While developing, if you want to replace a version of a migration that was only present in a prior version of your workspace, manually undo the changes made to
the schema, then either run the SQL "delete from migrations where id = 'TIMESTAMP'" or start using a new timestamp.

When a new version of powershelf_manager is run, all .up.sql migrations that were not present before are run in a single transaction in the order of their timestamps.



