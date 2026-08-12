-- Nothing in this file declares Email's type. Without a live catalog there is
-- no honest way to know it, so the rule must stay silent rather than guess.
DECLARE @Email nvarchar(200);
SELECT CustomerId FROM dbo.Customers WHERE Email = @Email;
