# mockdpa

A program to mock the DPA functionality. It responds to heartbeats and SetVni config messages

## Usage

1. Start up `mosquitto` (or some MQTT broker) locally or use the EMQX container already started
2. run `./mockdpa -h` for usage.
3. run mockdpa, specifying the proper host name and port numbers for the MQTT broker
