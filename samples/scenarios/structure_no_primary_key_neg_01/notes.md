Same 240k-row table, now with a clustered PRIMARY KEY on a narrow surrogate
(ShipmentId). It is not a heap and the key is a single column, so none of the
structure rules should fire.
