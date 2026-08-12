-- decimal is exact, so currency arithmetic does not drift.
CREATE TABLE dbo.Invoices (InvoiceId int NOT NULL, TotalAmount decimal(19, 4) NOT NULL);
