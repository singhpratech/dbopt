-- The concatenated columns are on the right, but the left side is a variable:
-- the columns are still the wrapped side and the seek is still lost.
SELECT PersonId FROM dbo.Person WHERE @full = FirstName + ' ' + LastName;
