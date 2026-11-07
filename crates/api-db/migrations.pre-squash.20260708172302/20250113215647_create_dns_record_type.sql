CREATE TABLE dns_record_types (
    id SERIAL PRIMARY KEY,
    type_name VARCHAR(10) UNIQUE NOT NULL
);

INSERT INTO dns_record_types (type_name) VALUES
('A'),
('AAAA'),
('CNAME'),
('MX'),
('NS'),
('SRV'),
('TXT');

