-- Executed once by the official PostgreSQL image when the data volume is empty.
CREATE SCHEMA inventory;

CREATE TABLE public.customers (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL,
    email text UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE inventory.products (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL,
    price numeric(10, 2) NOT NULL CHECK (price >= 0),
    in_stock boolean NOT NULL DEFAULT true
);

CREATE TABLE public.orders (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id bigint NOT NULL REFERENCES public.customers(id),
    product_id bigint NOT NULL REFERENCES inventory.products(id),
    quantity integer NOT NULL CHECK (quantity > 0),
    ordered_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO public.customers (name, email) VALUES
    ('Ada Lovelace', 'ada@example.test'),
    ('Grace Hopper', 'grace@example.test'),
    ('Linus Torvalds', NULL);

INSERT INTO inventory.products (name, price, in_stock) VALUES
    ('Notebook', 12.50, true),
    ('Mechanical keyboard', 89.00, true),
    ('USB-C cable', 7.25, false);

INSERT INTO public.orders (customer_id, product_id, quantity) VALUES
    (1, 1, 2),
    (1, 2, 1),
    (2, 3, 3);

CREATE VIEW inventory.available_products AS
    SELECT id, name, price FROM inventory.products WHERE in_stock;

CREATE MATERIALIZED VIEW inventory.stock_summary AS
    SELECT in_stock, count(*) AS product_count
    FROM inventory.products GROUP BY in_stock;

CREATE FUNCTION inventory.product_label(product_id bigint) RETURNS text
    LANGUAGE sql STABLE AS $$
        SELECT name FROM inventory.products WHERE id = product_id
    $$;

CREATE FUNCTION inventory.product_label(product_name text) RETURNS text
    LANGUAGE sql IMMUTABLE AS $$
        SELECT product_name
    $$;

CREATE PROCEDURE inventory.refresh_stock_summary()
    LANGUAGE sql AS $$ REFRESH MATERIALIZED VIEW inventory.stock_summary $$;
