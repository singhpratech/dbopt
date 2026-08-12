-- The BULK form reads a file and carries no credentials at all.
SELECT * FROM OPENROWSET(BULK 'C:\data\orders.csv', SINGLE_CLOB) AS r;
