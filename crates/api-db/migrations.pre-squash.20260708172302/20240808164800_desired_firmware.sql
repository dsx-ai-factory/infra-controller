CREATE TABLE desired_firmware (
    vendor TEXT NOT NULL,
    model TEXT NOT NULL,
    versions TEXT NOT NULL,

    PRIMARY KEY (vendor, model)
);

