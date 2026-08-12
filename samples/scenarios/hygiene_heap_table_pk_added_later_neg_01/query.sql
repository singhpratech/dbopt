-- Every scripted schema in existence creates the table first and adds the
-- clustered key in a later statement. Looking only inside CREATE TABLE called
-- essentially every table in a real schema a heap.
CREATE TABLE [Person].[Address](
    [AddressID] int NOT NULL,
    [City] nvarchar(30) NOT NULL
);
GO
ALTER TABLE [Person].[Address] ADD CONSTRAINT [PK_Address_AddressID]
    PRIMARY KEY CLUSTERED ([AddressID] ASC);
GO
