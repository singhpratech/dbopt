-- The primary key on (ProductID, LocationID) is declared by an ALTER TABLE in an
-- earlier batch of the same file. The query filters on exactly that key, so
-- recommending a new index would be advice against an index that exists.
ALTER TABLE [Production].[ProductInventory] WITH CHECK ADD
    CONSTRAINT [PK_ProductInventory_ProductID_LocationID] PRIMARY KEY CLUSTERED
    (
    [ProductID],
    [LocationID]
    )  ON [PRIMARY];
GO

CREATE PROCEDURE [dbo].[ufnGetStock] @ProductID int
AS
BEGIN
    SET NOCOUNT ON;
    DECLARE @ret int;
    SELECT @ret = SUM(p.[Quantity])
    FROM   [Production].[ProductInventory] AS p
    WHERE  p.[ProductID] = @ProductID
      AND  p.[LocationID] = '6';
    RETURN @ret;
END;
GO
