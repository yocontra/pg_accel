CREATE TABLE tpch_region (r_regionkey int4 PRIMARY KEY, r_name text NOT NULL, r_comment text NOT NULL);
CREATE TABLE tpch_nation (n_nationkey int4 PRIMARY KEY, n_name text NOT NULL, n_regionkey int4 NOT NULL REFERENCES tpch_region, n_comment text NOT NULL);
CREATE TABLE tpch_supplier (s_suppkey int8 PRIMARY KEY, s_name text NOT NULL, s_address text NOT NULL, s_nationkey int4 NOT NULL REFERENCES tpch_nation, s_phone text NOT NULL, s_acctbal numeric(15,2) NOT NULL, s_comment text NOT NULL);
CREATE TABLE tpch_customer (c_custkey int8 PRIMARY KEY, c_name text NOT NULL, c_address text NOT NULL, c_nationkey int4 NOT NULL REFERENCES tpch_nation, c_phone text NOT NULL, c_acctbal numeric(15,2) NOT NULL, c_mktsegment text NOT NULL, c_comment text NOT NULL);
CREATE TABLE tpch_part (p_partkey int8 PRIMARY KEY, p_name text NOT NULL, p_mfgr text NOT NULL, p_brand text NOT NULL, p_type text NOT NULL, p_size int4 NOT NULL, p_container text NOT NULL, p_retailprice numeric(15,2) NOT NULL, p_comment text NOT NULL);
CREATE TABLE tpch_partsupp (ps_partkey int8 NOT NULL REFERENCES tpch_part, ps_suppkey int8 NOT NULL REFERENCES tpch_supplier, ps_availqty int4 NOT NULL, ps_supplycost numeric(15,2) NOT NULL, ps_comment text NOT NULL, PRIMARY KEY (ps_partkey, ps_suppkey));
CREATE TABLE tpch_orders (o_orderkey int8 PRIMARY KEY, o_custkey int8 NOT NULL REFERENCES tpch_customer, o_orderstatus text NOT NULL, o_totalprice numeric(15,2) NOT NULL, o_orderdate date NOT NULL, o_orderpriority text NOT NULL, o_clerk text NOT NULL, o_shippriority int4 NOT NULL, o_comment text NOT NULL);
CREATE TABLE tpch_lineitem (l_orderkey int8 NOT NULL REFERENCES tpch_orders, l_partkey int8 NOT NULL, l_suppkey int8 NOT NULL, l_linenumber int4 NOT NULL, l_quantity numeric(15,2) NOT NULL, l_extendedprice numeric(15,2) NOT NULL, l_discount numeric(15,4) NOT NULL, l_tax numeric(15,4) NOT NULL, l_returnflag text NOT NULL, l_linestatus text NOT NULL, l_shipdate date NOT NULL, l_commitdate date NOT NULL, l_receiptdate date NOT NULL, l_shipinstruct text NOT NULL, l_shipmode text NOT NULL, l_comment text NOT NULL, PRIMARY KEY (l_orderkey, l_linenumber), FOREIGN KEY (l_partkey, l_suppkey) REFERENCES tpch_partsupp);
INSERT INTO tpch_region VALUES (0,'AFRICA','region 0'),(1,'AMERICA','region 1'),(2,'ASIA','region 2'),(3,'EUROPE','region 3'),(4,'MIDDLE EAST','region 4');
INSERT INTO tpch_nation SELECT i::int4, name, region, 'nation ' || i FROM (VALUES (0,'ALGERIA',0),(1,'ARGENTINA',1),(2,'BRAZIL',1),(3,'CANADA',1),(4,'EGYPT',4),(5,'ETHIOPIA',0),(6,'FRANCE',3),(7,'GERMANY',3),(8,'INDIA',2),(9,'INDONESIA',2),(10,'IRAN',4),(11,'IRAQ',4),(12,'JAPAN',2),(13,'JORDAN',4),(14,'KENYA',0),(15,'MOROCCO',0),(16,'MOZAMBIQUE',0),(17,'PERU',1),(18,'CHINA',2),(19,'ROMANIA',3),(20,'SAUDI ARABIA',4),(21,'VIETNAM',2),(22,'RUSSIA',3),(23,'UNITED KINGDOM',3),(24,'UNITED STATES',1)) n(i,name,region);
INSERT INTO tpch_supplier SELECT i, 'Supplier#' || lpad(i::text,9,'0'), 'supplier address ' || i, (i % 25)::int4, (10+i%25)::text || '-100-200-3000', ((i*193)%1100000-100000)::numeric/100, CASE WHEN i%19=0 THEN 'Customer pending Complaints' ELSE 'supplier comment ' || i END FROM generate_series(1::int8,100) g(i);
INSERT INTO tpch_customer SELECT i, 'Customer#' || lpad(i::text,9,'0'), 'customer address ' || i, (i%25)::int4, (10+i%25)::text || '-400-500-6000', ((i*719)%1100000-100000)::numeric/100, (ARRAY['AUTOMOBILE','BUILDING','FURNITURE','HOUSEHOLD','MACHINERY'])[i%5+1], 'customer comment ' || i FROM generate_series(1::int8,2500) g(i);
INSERT INTO tpch_part SELECT i, (ARRAY['forest green','almond blue','green steel','ivory red'])[i%4+1] || ' part ' || i, 'Manufacturer#' || (i%5+1), (ARRAY['Brand#12','Brand#23','Brand#34','Brand#45','Brand#55'])[i%5+1], (ARRAY['STANDARD ANODIZED STEEL','SMALL BRUSHED BRASS','PROMO BURNISHED COPPER','MEDIUM POLISHED TIN','LARGE PLATED BRASS','ECONOMY ANODIZED STEEL'])[i%6+1], (i%50+1)::int4, (ARRAY['SM CASE','SM BOX','SM PACK','SM PKG','MED BAG','MED BOX','MED PKG','MED PACK','LG CASE','LG BOX','LG PACK','LG PKG'])[i%12+1], ((i*113)%900000+10000)::numeric/100, 'part comment ' || i FROM generate_series(1::int8,5000) g(i);
INSERT INTO tpch_partsupp SELECT p_partkey, (p_partkey+slot*7)%100+1, (1000+(p_partkey*31+slot)%9000)::int4, ((p_partkey*97+slot*13)%100000+100)::numeric/100, 'partsupp comment' FROM tpch_part CROSS JOIN generate_series(0,3) s(slot);
INSERT INTO tpch_orders SELECT i, 2*((i*17)%(2500//2))+1, CASE WHEN i%3=0 THEN 'O' ELSE 'F' END, ((i*7919)%900000+10000)::numeric/100, date '1992-01-01'+((i*17)%2400)::int4, (ARRAY['1-URGENT','2-HIGH','3-MEDIUM','4-NOT SPECIFIED','5-LOW'])[i%5+1], 'Clerk#' || (i%100+1), (i%2)::int4, CASE WHEN i%13=0 THEN 'special pending requests' ELSE 'order comment ' || i END FROM generate_series(1::int8,25000) g(i);
INSERT INTO tpch_lineitem SELECT o_orderkey, k.partkey, (k.partkey+((i-1)%4)*7)%100+1, ((i-1)%4+1)::int4, ((i*37+i//97)%100+1)::numeric(15,2), (((i*7919)%900000+10000)::numeric/100)::numeric(15,2), (((i*3)%11)::numeric/100)::numeric(15,4), (((i*5)%9)::numeric/100)::numeric(15,4), (ARRAY['A','N','R'])[i%3+1], (ARRAY['F','O'])[i%2+1], d.shipdate, d.shipdate+5, d.shipdate+CASE WHEN (i-1)%4=0 THEN 8 ELSE 3 END, (ARRAY['DELIVER IN PERSON','COLLECT COD','TAKE BACK RETURN'])[i%3+1], (ARRAY['AIR','FOB','MAIL','RAIL','REG AIR','SHIP','TRUCK'])[i%7+1], 'line comment ' || i FROM generate_series(1::int8,100000) g(i) JOIN tpch_orders ON o_orderkey=(i-1)//4+1 CROSS JOIN LATERAL (SELECT (i*13)%5000+1 AS partkey) k CROSS JOIN LATERAL (SELECT o_orderdate+(i%121+1)::int4 AS shipdate) d;
CREATE INDEX tpch_orders_customer ON tpch_orders (o_custkey);
CREATE INDEX tpch_lineitem_part_supplier ON tpch_lineitem (l_partkey,l_suppkey);
CREATE INDEX tpch_lineitem_supplier ON tpch_lineitem (l_suppkey);
ANALYZE tpch_region;
ANALYZE tpch_nation;
ANALYZE tpch_supplier;
ANALYZE tpch_customer;
ANALYZE tpch_part;
ANALYZE tpch_partsupp;
ANALYZE tpch_orders;
ANALYZE tpch_lineitem;

SELECT 1 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT l_returnflag, l_linestatus, SUM(l_quantity) AS sum_qty, SUM(l_extendedprice) AS sum_base_price, SUM(l_extendedprice * (1 - l_discount)) AS sum_disc_price, SUM(l_extendedprice * (1 - l_discount) * (1 + l_tax)) AS sum_charge, AVG(l_quantity) AS avg_qty, AVG(l_extendedprice) AS avg_price, AVG(l_discount) AS avg_disc, COUNT(*) AS count_order FROM tpch_lineitem WHERE l_shipdate <= date '1998-12-01' - 90 GROUP BY l_returnflag, l_linestatus ORDER BY l_returnflag, l_linestatus) q;

SELECT 2 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT s_acctbal, s_name, n_name, p_partkey, p_mfgr, s_address, s_phone, s_comment
FROM tpch_part JOIN tpch_partsupp ON ps_partkey=p_partkey
JOIN tpch_supplier ON s_suppkey=ps_suppkey
JOIN tpch_nation ON n_nationkey=s_nationkey
JOIN tpch_region ON r_regionkey=n_regionkey
WHERE p_size=15 AND p_type LIKE '%BRASS' AND r_name='EUROPE'
AND ps_supplycost=(SELECT min(x.ps_supplycost) FROM tpch_partsupp x
  JOIN tpch_supplier s ON s.s_suppkey=x.ps_suppkey
  JOIN tpch_nation n ON n.n_nationkey=s.s_nationkey
  JOIN tpch_region r ON r.r_regionkey=n.n_regionkey
  WHERE x.ps_partkey=p_partkey AND r.r_name='EUROPE')
ORDER BY s_acctbal DESC, n_name, s_name, p_partkey LIMIT 100) q;

SELECT 3 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT l_orderkey, sum(l_extendedprice*(1-l_discount)) AS revenue, o_orderdate, o_shippriority
FROM tpch_customer JOIN tpch_orders ON o_custkey=c_custkey
JOIN tpch_lineitem ON l_orderkey=o_orderkey
WHERE c_mktsegment='BUILDING' AND o_orderdate<date '1995-03-15' AND l_shipdate>date '1995-03-15'
GROUP BY l_orderkey,o_orderdate,o_shippriority
ORDER BY revenue DESC,o_orderdate,l_orderkey LIMIT 10) q;

SELECT 4 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT o_orderpriority,count(*) AS order_count FROM tpch_orders
WHERE o_orderdate>=date '1993-07-01' AND o_orderdate<date '1993-10-01'
AND EXISTS (SELECT 1 FROM tpch_lineitem WHERE l_orderkey=o_orderkey AND l_commitdate<l_receiptdate)
GROUP BY o_orderpriority ORDER BY o_orderpriority) q;

SELECT 5 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT n_name,sum(l_extendedprice*(1-l_discount)) AS revenue
FROM tpch_customer JOIN tpch_orders ON o_custkey=c_custkey
JOIN tpch_lineitem ON l_orderkey=o_orderkey
JOIN tpch_supplier ON s_suppkey=l_suppkey AND s_nationkey=c_nationkey
JOIN tpch_nation ON n_nationkey=s_nationkey
JOIN tpch_region ON r_regionkey=n_regionkey
WHERE r_name='ASIA' AND o_orderdate>=date '1994-01-01' AND o_orderdate<date '1995-01-01'
GROUP BY n_name ORDER BY revenue DESC,n_name) q;

SELECT 6 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT SUM(l_extendedprice * l_discount) AS revenue FROM tpch_lineitem WHERE l_shipdate >= date '1994-01-01' AND l_shipdate < date '1995-01-01' AND l_discount BETWEEN 0.05 AND 0.07 AND l_quantity < 24) q;

SELECT 7 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT supplier_nation,customer_nation,ship_year,sum(volume) AS revenue FROM (
 SELECT sn.n_name AS supplier_nation,cn.n_name AS customer_nation,
 extract(year FROM l_shipdate) AS ship_year,l_extendedprice*(1-l_discount) AS volume
 FROM tpch_supplier JOIN tpch_lineitem ON l_suppkey=s_suppkey
 JOIN tpch_orders ON o_orderkey=l_orderkey JOIN tpch_customer ON c_custkey=o_custkey
 JOIN tpch_nation sn ON sn.n_nationkey=s_nationkey JOIN tpch_nation cn ON cn.n_nationkey=c_nationkey
 WHERE ((sn.n_name='FRANCE' AND cn.n_name='GERMANY') OR (sn.n_name='GERMANY' AND cn.n_name='FRANCE'))
 AND l_shipdate BETWEEN date '1995-01-01' AND date '1996-12-31'
) shipping GROUP BY supplier_nation,customer_nation,ship_year ORDER BY supplier_nation,customer_nation,ship_year) q;

SELECT 8 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT order_year,sum(CASE WHEN supplier_nation='BRAZIL' THEN volume ELSE 0 END)/sum(volume) AS market_share FROM (
 SELECT extract(year FROM o_orderdate) AS order_year,l_extendedprice*(1-l_discount) AS volume,sn.n_name AS supplier_nation
 FROM tpch_part JOIN tpch_lineitem ON l_partkey=p_partkey JOIN tpch_supplier ON s_suppkey=l_suppkey
 JOIN tpch_orders ON o_orderkey=l_orderkey JOIN tpch_customer ON c_custkey=o_custkey
 JOIN tpch_nation cn ON cn.n_nationkey=c_nationkey JOIN tpch_region ON r_regionkey=cn.n_regionkey
 JOIN tpch_nation sn ON sn.n_nationkey=s_nationkey
 WHERE r_name='AMERICA' AND o_orderdate BETWEEN date '1995-01-01' AND date '1996-12-31'
 AND p_type='STANDARD ANODIZED STEEL'
) sales GROUP BY order_year ORDER BY order_year) q;

SELECT 9 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT nation,order_year,sum(amount) AS sum_profit FROM (
 SELECT n_name AS nation,extract(year FROM o_orderdate) AS order_year,
 l_extendedprice*(1-l_discount)-ps_supplycost*l_quantity AS amount
 FROM tpch_part JOIN tpch_lineitem ON l_partkey=p_partkey JOIN tpch_supplier ON s_suppkey=l_suppkey
 JOIN tpch_partsupp ON ps_partkey=l_partkey AND ps_suppkey=l_suppkey
 JOIN tpch_orders ON o_orderkey=l_orderkey JOIN tpch_nation ON n_nationkey=s_nationkey
 WHERE p_name LIKE '%green%'
) profits GROUP BY nation,order_year ORDER BY nation,order_year DESC) q;

SELECT 10 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT c_custkey,c_name,sum(l_extendedprice*(1-l_discount)) AS revenue,c_acctbal,n_name,c_address,c_phone,c_comment
FROM tpch_customer JOIN tpch_orders ON o_custkey=c_custkey JOIN tpch_lineitem ON l_orderkey=o_orderkey
JOIN tpch_nation ON n_nationkey=c_nationkey
WHERE o_orderdate>=date '1993-10-01' AND o_orderdate<date '1994-01-01' AND l_returnflag='R'
GROUP BY c_custkey,c_name,c_acctbal,n_name,c_address,c_phone,c_comment
ORDER BY revenue DESC,c_custkey LIMIT 20) q;

SELECT 11 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT ps_partkey,sum(ps_supplycost*ps_availqty) AS value
FROM tpch_partsupp JOIN tpch_supplier ON s_suppkey=ps_suppkey JOIN tpch_nation ON n_nationkey=s_nationkey
WHERE n_name='GERMANY' GROUP BY ps_partkey
HAVING sum(ps_supplycost*ps_availqty)>(SELECT sum(ps_supplycost*ps_availqty)*0.0001
 FROM tpch_partsupp JOIN tpch_supplier ON s_suppkey=ps_suppkey JOIN tpch_nation ON n_nationkey=s_nationkey
 WHERE n_name='GERMANY') ORDER BY value DESC,ps_partkey) q;

SELECT 12 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT l_shipmode, SUM(CASE WHEN o_orderpriority IN ('1-URGENT', '2-HIGH') THEN 1 ELSE 0 END) AS high_line_count, SUM(CASE WHEN o_orderpriority NOT IN ('1-URGENT', '2-HIGH') THEN 1 ELSE 0 END) AS low_line_count FROM tpch_orders JOIN tpch_lineitem ON o_orderkey = l_orderkey WHERE l_shipmode IN ('MAIL', 'SHIP') AND l_commitdate < l_receiptdate AND l_shipdate < l_commitdate AND l_receiptdate >= date '1994-01-01' AND l_receiptdate < date '1995-01-01' GROUP BY l_shipmode ORDER BY l_shipmode) q;

SELECT 13 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT order_count,count(*) AS customer_count FROM (
 SELECT c_custkey,count(o_orderkey) AS order_count FROM tpch_customer
 LEFT JOIN tpch_orders ON o_custkey=c_custkey AND o_comment NOT LIKE '%special%requests%'
 GROUP BY c_custkey
) distribution GROUP BY order_count ORDER BY customer_count DESC,order_count DESC) q;

SELECT 14 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT 100.00*sum(CASE WHEN p_type LIKE 'PROMO%' THEN l_extendedprice*(1-l_discount) ELSE 0 END)
/sum(l_extendedprice*(1-l_discount)) AS promo_revenue
FROM tpch_lineitem JOIN tpch_part ON p_partkey=l_partkey
WHERE l_shipdate>=date '1995-09-01' AND l_shipdate<date '1995-10-01') q;

SELECT 15 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (WITH revenue AS (
 SELECT l_suppkey AS supplier_key,sum(l_extendedprice*(1-l_discount)) AS total_revenue
 FROM tpch_lineitem WHERE l_shipdate>=date '1996-01-01' AND l_shipdate<date '1996-04-01' GROUP BY l_suppkey
)
SELECT s_suppkey,s_name,s_address,s_phone,total_revenue FROM tpch_supplier JOIN revenue ON supplier_key=s_suppkey
WHERE total_revenue=(SELECT max(total_revenue) FROM revenue) ORDER BY s_suppkey) q;

SELECT 16 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT p_brand,p_type,p_size,count(DISTINCT ps_suppkey) AS supplier_count
FROM tpch_partsupp JOIN tpch_part ON p_partkey=ps_partkey
WHERE p_brand<>'Brand#45' AND p_type NOT LIKE 'MEDIUM POLISHED%'
AND p_size IN (49,14,23,45,19,3,36,9)
AND ps_suppkey NOT IN (SELECT s_suppkey FROM tpch_supplier WHERE s_comment LIKE '%Customer%Complaints%')
GROUP BY p_brand,p_type,p_size ORDER BY supplier_count DESC,p_brand,p_type,p_size) q;

SELECT 17 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT sum(l_extendedprice)/7.0 AS avg_yearly FROM tpch_lineitem JOIN tpch_part ON p_partkey=l_partkey
WHERE p_brand='Brand#23' AND p_container='MED BOX'
AND l_quantity<(SELECT 0.2*avg(x.l_quantity) FROM tpch_lineitem x WHERE x.l_partkey=p_partkey)) q;

SELECT 18 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT c_name,c_custkey,o_orderkey,o_orderdate,o_totalprice,sum(l_quantity) AS quantity
FROM tpch_customer JOIN tpch_orders ON o_custkey=c_custkey JOIN tpch_lineitem ON l_orderkey=o_orderkey
WHERE o_orderkey IN (SELECT l_orderkey FROM tpch_lineitem GROUP BY l_orderkey HAVING sum(l_quantity)>300)
GROUP BY c_name,c_custkey,o_orderkey,o_orderdate,o_totalprice
ORDER BY o_totalprice DESC,o_orderdate,o_orderkey LIMIT 100) q;

SELECT 19 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT sum(l_extendedprice*(1-l_discount)) AS revenue FROM tpch_lineitem JOIN tpch_part ON p_partkey=l_partkey
WHERE l_shipmode IN ('AIR','REG AIR') AND l_shipinstruct='DELIVER IN PERSON' AND (
 (p_brand='Brand#12' AND p_container IN ('SM CASE','SM BOX','SM PACK','SM PKG') AND l_quantity BETWEEN 1 AND 11 AND p_size BETWEEN 1 AND 5)
 OR (p_brand='Brand#23' AND p_container IN ('MED BAG','MED BOX','MED PKG','MED PACK') AND l_quantity BETWEEN 10 AND 20 AND p_size BETWEEN 1 AND 10)
 OR (p_brand='Brand#34' AND p_container IN ('LG CASE','LG BOX','LG PACK','LG PKG') AND l_quantity BETWEEN 20 AND 30 AND p_size BETWEEN 1 AND 15))) q;

SELECT 20 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT s_name,s_address FROM tpch_supplier JOIN tpch_nation ON n_nationkey=s_nationkey
WHERE n_name='CANADA' AND s_suppkey IN (
 SELECT ps_suppkey FROM tpch_partsupp WHERE ps_partkey IN (SELECT p_partkey FROM tpch_part WHERE p_name LIKE 'forest%')
 AND ps_availqty>(SELECT 0.5*sum(l_quantity) FROM tpch_lineitem
 WHERE l_partkey=ps_partkey AND l_suppkey=ps_suppkey AND l_shipdate>=date '1994-01-01' AND l_shipdate<date '1995-01-01'))
ORDER BY s_name) q;

SELECT 21 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT s_name,count(*) AS numwait FROM tpch_supplier JOIN tpch_lineitem l ON l.l_suppkey=s_suppkey
JOIN tpch_orders ON o_orderkey=l.l_orderkey JOIN tpch_nation ON n_nationkey=s_nationkey
WHERE o_orderstatus='F' AND l.l_receiptdate>l.l_commitdate AND n_name='SAUDI ARABIA'
AND EXISTS (SELECT 1 FROM tpch_lineitem x WHERE x.l_orderkey=l.l_orderkey AND x.l_suppkey<>l.l_suppkey)
AND NOT EXISTS (SELECT 1 FROM tpch_lineitem x WHERE x.l_orderkey=l.l_orderkey AND x.l_suppkey<>l.l_suppkey AND x.l_receiptdate>x.l_commitdate)
GROUP BY s_name ORDER BY numwait DESC,s_name LIMIT 100) q;

SELECT 22 AS query, count(*) AS rows, count(*) FILTER (WHERE to_json(q)::text NOT LIKE '%:null%') AS nonnull_rows FROM (SELECT substring(c_phone FROM 1 FOR 2) AS country_code,count(*) AS numcust,sum(c_acctbal) AS totacctbal
FROM tpch_customer WHERE substring(c_phone FROM 1 FOR 2) IN ('13','31','23','29','30','18','17')
AND c_acctbal>(SELECT avg(c_acctbal) FROM tpch_customer WHERE c_acctbal>0 AND substring(c_phone FROM 1 FOR 2) IN ('13','31','23','29','30','18','17'))
AND NOT EXISTS (SELECT 1 FROM tpch_orders WHERE o_custkey=c_custkey)
GROUP BY substring(c_phone FROM 1 FOR 2) ORDER BY country_code) q;
