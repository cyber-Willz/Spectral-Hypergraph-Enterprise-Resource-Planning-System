-- Synthetic 90-week history with planted structure for exercising the cognitive layer.
-- Planted: seasonal demand (13-week cycle), supplier S3 lead-time drift, supplier S2 rising defect rate,
-- carrier K2 growing delays, two products whose resupply stops in the last 6 weeks.
-- Usage: psql "$DATABASE_URL" -f scripts/seed_cognitive_demo.sql   (run on an empty database after migrations)
SELECT setseed(0.42);
DO $$
DECLARE
  wk0 date := date_trunc('week', current_date)::date - 7 * 90;
  wh uuid[]; loc uuid[]; sup uuid[]; cus uuid[]; car uuid[]; prod uuid[]; w int; p int; c int; s int;
  d date; q int; base int; so uuid; po uuid; pol uuid; sh uuid; lt int; late numeric; rej int; cost numeric; k int; cr int;
  ts timestamptz; shipped timestamptz;
BEGIN
  FOR k IN 1..3 LOOP
    wh := wh || gen_random_uuid(); loc := loc || gen_random_uuid();
    INSERT INTO warehouses (id, code, name) VALUES (wh[k], 'W' || k, 'Warehouse ' || k);
    INSERT INTO locations (id, warehouse_id, code, capacity) VALUES (loc[k], wh[k], 'BIN-1', CASE WHEN k = 1 THEN 400000 ELSE NULL END);
  END LOOP;
  FOR k IN 1..4 LOOP
    sup := sup || gen_random_uuid();
    INSERT INTO suppliers (id, code, name, lead_time_days) VALUES (sup[k], 'S' || k, 'Supplier ' || k, 7 + 3 * k);
  END LOOP;
  FOR k IN 1..6 LOOP
    cus := cus || gen_random_uuid(); INSERT INTO scm_customers (id, code, name) VALUES (cus[k], 'C' || k, 'Customer ' || k);
  END LOOP;
  FOR k IN 1..3 LOOP
    car := car || gen_random_uuid(); INSERT INTO carriers (id, code, name) VALUES (car[k], 'K' || k, 'Carrier ' || k);
  END LOOP;
  FOR k IN 1..12 LOOP
    prod := prod || gen_random_uuid();
    INSERT INTO products (id, sku, name, unit_cost, list_price, reorder_point, reorder_qty, lead_time_days, criticality_class)
    VALUES (prod[k], 'P' || lpad(k::text, 2, '0'), 'Product ' || k, 5 + k, 9 + 2 * k, 100, 500, 7 + (k % 4) * 3, CASE WHEN k <= 3 THEN 'A' WHEN k <= 8 THEN 'B' ELSE 'C' END);
    -- two approved sources per product
    INSERT INTO product_suppliers (id, product_id, supplier_id, is_preferred, unit_cost, lead_time_days, min_order_qty, order_multiple)
      VALUES (gen_random_uuid(), prod[k], sup[1 + (k % 4)], TRUE, 5 + k, 7 + (k % 4) * 3, 50, 10);
    INSERT INTO product_suppliers (id, product_id, supplier_id, unit_cost, lead_time_days, min_order_qty, order_multiple)
      VALUES (gen_random_uuid(), prod[k], sup[1 + ((k + 1) % 4)], (5 + k) * 1.06, 10 + (k % 4) * 3, 100, 10);
  END LOOP;
  INSERT INTO contracts (id, supplier_id, contract_number, start_date, end_date) VALUES (gen_random_uuid(), sup[1], 'CT-1', current_date - 900, current_date + 400);
  -- opening stock
  FOR p IN 1..12 LOOP
    FOR k IN 1..3 LOOP
      INSERT INTO stock_movements (id, product_id, location_id, movement_type, quantity_delta, created_at)
        VALUES (gen_random_uuid(), prod[p], loc[k], 'adjustment', CASE WHEN p IN (11, 12) THEN 3000 ELSE 4000 END, wk0::timestamptz - interval '1 day');
    END LOOP;
  END LOOP;

  FOR w IN 0..89 LOOP
    d := wk0 + 7 * w;
    -- sales: one order per customer per week with several lines
    FOR c IN 1..6 LOOP
      so := gen_random_uuid(); ts := d::timestamptz + ((c % 5) || ' days')::interval + interval '9 hours';
      k := 1 + ((w + c) % 3);
      INSERT INTO sales_orders (id, so_number, customer_id, warehouse_id, status, created_at, updated_at)
        VALUES (so, 'SO-' || w || '-' || c, cus[c], wh[k], 'shipped', ts, ts);
      FOR p IN 1..12 LOOP
        IF (p + c + w) % 3 = 0 THEN
          base := 20 + 6 * p;
          q := GREATEST(1, round(base * (1 + 0.3 * sin(2 * pi() * w / 13.0)) * (0.7 + 0.6 * random()) * (CASE WHEN p <= 2 THEN 1 + w * 0.004 ELSE 1 END)))::int;
          INSERT INTO sales_order_lines (id, sales_order_id, product_id, quantity, quantity_shipped, unit_price) VALUES (gen_random_uuid(), so, prod[p], q, q, 9 + 2 * p);
          INSERT INTO stock_movements (id, product_id, location_id, movement_type, quantity_delta, created_at)
            VALUES (gen_random_uuid(), prod[p], loc[k], 'issue', -q, ts);
        END IF;
      END LOOP;
      cr := 1 + floor(random() * 3)::int;
      shipped := ts + interval '1 day';
      sh := gen_random_uuid();
      INSERT INTO shipments (id, shipment_number, sales_order_id, carrier_id, status, shipped_at, delivered_at, expected_delivery_date, freight_cost, created_at)
        VALUES (sh, 'SH-' || w || '-' || c, so, car[cr], CASE WHEN cr = 3 AND random() < 0.06 THEN 'exception' ELSE 'delivered' END, shipped,
                shipped + ((3 + (CASE WHEN cr = 2 THEN w * 0.04 ELSE 0 END) + random() * 1.5) || ' days')::interval, (shipped + interval '3 days')::date,
                10 + 4 * cr, ts);
    END LOOP;
    -- purchasing: weekly PO per product from its primary supplier
    FOR p IN 1..12 LOOP
      IF NOT (p IN (11, 12) AND w >= 84) THEN
        s := 1 + (p % 4);
        lt := 7 + (p % 4) * 3;
        late := (random() * 1.5 - 0.5) + (CASE WHEN s = 3 THEN w * 0.07 ELSE 0 END);
        rej := CASE WHEN s = 2 THEN round(2 + w * 0.12 + random() * 2) ELSE round(random() * 2) END;
        q := round((20 + 6 * p) * 3.2 * (0.8 + 0.4 * random()))::int;
        po := gen_random_uuid(); pol := gen_random_uuid();
        INSERT INTO purchase_orders (id, po_number, supplier_id, warehouse_id, status, order_date, expected_date, created_at, updated_at)
          VALUES (po, 'PO-' || w || '-' || p, sup[s], wh[1 + (p % 3)], 'received', d - lt, d, (d - lt)::timestamptz, (d - lt)::timestamptz);
        INSERT INTO purchase_order_lines (id, purchase_order_id, product_id, quantity_ordered, quantity_received, unit_cost, promised_date)
          VALUES (pol, po, prod[p], q, q, 5 + p, d);
        INSERT INTO goods_receipts (id, purchase_order_id, purchase_order_line_id, product_id, location_id, quantity, received_on, rejected_qty)
          VALUES (gen_random_uuid(), po, pol, prod[p], loc[1 + (p % 3)], q, d + round(late)::int, LEAST(q, round(q * rej / 100.0))::int);
        INSERT INTO stock_movements (id, product_id, location_id, movement_type, quantity_delta, created_at)
          VALUES (gen_random_uuid(), prod[p], loc[1 + (p % 3)], 'receipt', q, (d + round(late)::int)::timestamptz);
      END IF;
    END LOOP;
  END LOOP;

  -- ledger -> stock levels
  INSERT INTO stock_levels (id, product_id, location_id, lot_number, quantity, reserved)
    SELECT gen_random_uuid(), product_id, location_id, '', GREATEST(SUM(quantity_delta), 0), 0 FROM stock_movements GROUP BY product_id, location_id;
END $$;
