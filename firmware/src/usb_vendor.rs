use astra918_firmware::astra as a;
use embassy_usb::{
    Builder,
    driver::{Driver, EndpointAddress},
    msos::{CompatibleIdFeatureDescriptor, PropertyData, RegistryPropertyFeatureDescriptor},
};
pub fn add<'d, D: Driver<'d>>(
    builder: &mut Builder<'d, D>,
) -> (D::EndpointOut, D::EndpointIn, D::EndpointIn) {
    builder.msos_descriptor(0x06030000, 0x20);
    let mut f = builder.function(0xff, 0, 0);
    f.msos_feature(CompatibleIdFeatureDescriptor::new("WINUSB", ""));
    f.msos_feature(RegistryPropertyFeatureDescriptor::new(
        "DeviceInterfaceGUIDs",
        PropertyData::RegMultiSz(&["{6968CA61-7B2B-4C57-A713-842D2F96C91A}"]),
    ));
    let mut interface = f.interface();
    let mut alt = interface.alt_setting(0xff, 0, 0, None);
    (
        alt.endpoint_bulk_out(Some(EndpointAddress::from(a::COMMAND_EP)), 64),
        alt.endpoint_bulk_in(Some(EndpointAddress::from(a::REPLY_EP)), 64),
        alt.endpoint_bulk_in(Some(EndpointAddress::from(a::IQ_EP)), 64),
    )
}
