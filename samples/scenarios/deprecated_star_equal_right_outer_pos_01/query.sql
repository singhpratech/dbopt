-- The `=*` right-outer form was never detected at all, despite the rule's own
-- comment claiming it was.
SELECT a.Id FROM dbo.A a, dbo.B b WHERE a.Id =* b.Id;
