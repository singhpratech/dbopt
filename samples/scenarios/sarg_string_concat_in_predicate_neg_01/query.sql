-- Compare each column on its own so both stay seekable.
SELECT CustomerId FROM dbo.Customers WHERE FirstName = 'Ada' AND LastName = 'Lovelace';
