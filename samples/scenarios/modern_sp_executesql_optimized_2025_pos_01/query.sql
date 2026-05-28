-- Properly parameterized dynamic SQL via sp_executesql. This is a hot code path
-- called thousands of times per minute; on 2025+ it should opt into
-- OPTIMIZED_SP_EXECUTESQL to avoid repeated compile work.
EXEC sp_executesql
    N'SELECT OrderId, Status FROM dbo.Orders WHERE CustomerId = @cid',
    N'@cid int',
    @cid = 42;
