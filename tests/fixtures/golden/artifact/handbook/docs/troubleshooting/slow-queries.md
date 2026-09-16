# Slow queries

Queries are slow when the database lacks the indexes the migrations create. Run
`service migrate` and check the query log for sequential scans.
